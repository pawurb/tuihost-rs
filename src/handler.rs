use crate::pty::{PtySession, PtyWriter};
use crate::server::CmdConfig;
use russh::server::{Auth, Handler, Msg, Session};
use russh::{Channel, ChannelId, CryptoVec, Disconnect};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::Mutex;
use tracing::{debug, error, info, warn};

const MIN_PTY_COLS: u16 = 10;
const MAX_PTY_COLS: u16 = 500;
const MIN_PTY_ROWS: u16 = 5;
const MAX_PTY_ROWS: u16 = 200;

pub struct SessionHandler {
    tui_config: Arc<CmdConfig>,
    pty_size: (u16, u16),
    pty_writers: Arc<Mutex<HashMap<ChannelId, Arc<Mutex<PtyWriter>>>>>,
    client_addr: String,
    active_connections: Arc<AtomicUsize>,
    shell_requested: bool,
}

impl SessionHandler {
    pub fn new(
        tui_config: Arc<CmdConfig>,
        client_addr: String,
        active_connections: Arc<AtomicUsize>,
    ) -> Self {
        Self {
            tui_config,
            pty_size: (80, 24),
            pty_writers: Arc::new(Mutex::new(HashMap::new())),
            client_addr,
            active_connections,
            shell_requested: false,
        }
    }

    fn clamp_pty_size(cols: u32, rows: u32) -> (u16, u16) {
        let cols = (cols as u16).clamp(MIN_PTY_COLS, MAX_PTY_COLS);
        let rows = (rows as u16).clamp(MIN_PTY_ROWS, MAX_PTY_ROWS);
        (cols, rows)
    }

}

impl Drop for SessionHandler {
    fn drop(&mut self) {
        let prev = self.active_connections.fetch_sub(1, Ordering::SeqCst);
        debug!(
            "Connection closed from {} ({} remaining)",
            self.client_addr,
            prev - 1
        );
    }
}

impl Handler for SessionHandler {
    type Error = russh::Error;

    async fn auth_none(&mut self, user: &str) -> Result<Auth, Self::Error> {
        info!(
            "Accepting anonymous auth for user: {} from {}",
            user, self.client_addr
        );
        Ok(Auth::Accept)
    }

    async fn auth_password(&mut self, user: &str, _password: &str) -> Result<Auth, Self::Error> {
        info!(
            "Accepting password auth for user: {} from {}",
            user, self.client_addr
        );
        Ok(Auth::Accept)
    }

    async fn auth_publickey(
        &mut self,
        user: &str,
        _public_key: &russh::keys::PublicKey,
    ) -> Result<Auth, Self::Error> {
        info!(
            "Accepting publickey auth for user: {} from {}",
            user, self.client_addr
        );
        Ok(Auth::Accept)
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        _session: &mut Session,
    ) -> Result<bool, Self::Error> {
        debug!(
            "Channel open session: {:?} from {}",
            channel.id(),
            self.client_addr
        );
        Ok(true)
    }

    async fn pty_request(
        &mut self,
        channel: ChannelId,
        _term: &str,
        col_width: u32,
        row_height: u32,
        _pix_width: u32,
        _pix_height: u32,
        _modes: &[(russh::Pty, u32)],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        let (cols, rows) = Self::clamp_pty_size(col_width, row_height);
        debug!(
            "PTY request for channel {:?}: {}x{} (requested {}x{}) from {}",
            channel, cols, rows, col_width, row_height, self.client_addr
        );
        self.pty_size = (cols, rows);
        session.channel_success(channel)?;
        Ok(())
    }

    async fn shell_request(
        &mut self,
        channel: ChannelId,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        if self.shell_requested {
            error!(
                "SECURITY: duplicate shell request from {} - disconnecting",
                self.client_addr
            );
            session.disconnect(Disconnect::ByApplication, "duplicate shell request", "en")?;
            return Ok(());
        }
        self.shell_requested = true;

        info!(
            "Shell request for channel {:?} from {}",
            channel, self.client_addr
        );

        let (cols, rows) = self.pty_size;
        let pty = match PtySession::spawn(
            &self.tui_config.command,
            &self.tui_config.args,
            &self.tui_config.env,
            cols,
            rows,
        ) {
            Ok(pty) => pty,
            Err(e) => {
                error!("Failed to spawn PTY for {}: {}", self.client_addr, e);
                session.channel_failure(channel)?;
                return Ok(());
            }
        };

        session.channel_success(channel)?;

        let (mut pty_reader, pty_writer) = pty.split();
        let pty_writer = Arc::new(Mutex::new(pty_writer));

        self.pty_writers
            .lock()
            .await
            .insert(channel, pty_writer.clone());

        let handle = session.handle();
        let client_addr = self.client_addr.clone();

        tokio::spawn(async move {
            let mut buf = [0u8; 4096];
            loop {
                match pty_reader.read(&mut buf).await {
                    Ok(0) => {
                        debug!("PTY closed (EOF) for {}", client_addr);
                        let _ = handle.close(channel).await;
                        break;
                    }
                    Ok(n) => {
                        let data = CryptoVec::from_slice(&buf[..n]);
                        if handle.data(channel, data).await.is_err() {
                            debug!(
                                "Failed to send data to channel for {}, closing",
                                client_addr
                            );
                            break;
                        }
                    }
                    Err(e) => {
                        if e.kind() != std::io::ErrorKind::WouldBlock {
                            debug!("PTY read error for {}: {}", client_addr, e);
                            let _ = handle.close(channel).await;
                            break;
                        }
                    }
                }
            }
        });

        Ok(())
    }

    async fn data(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        if let Some(writer) = self.pty_writers.lock().await.get(&channel) {
            let mut writer = writer.lock().await;
            if let Err(e) = writer.write_all(data).await {
                warn!("Failed to write to PTY for {}: {}", self.client_addr, e);
            }
        }
        Ok(())
    }

    async fn window_change_request(
        &mut self,
        channel: ChannelId,
        col_width: u32,
        row_height: u32,
        _pix_width: u32,
        _pix_height: u32,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        let (cols, rows) = Self::clamp_pty_size(col_width, row_height);
        debug!(
            "Window change for channel {:?}: {}x{} from {}",
            channel, cols, rows, self.client_addr
        );

        if let Some(writer) = self.pty_writers.lock().await.get(&channel) {
            let writer = writer.lock().await;
            if let Err(e) = writer.resize(cols, rows) {
                warn!("Failed to resize PTY for {}: {}", self.client_addr, e);
            }
        }
        Ok(())
    }

    async fn channel_close(
        &mut self,
        channel: ChannelId,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        debug!("Channel close: {:?} from {}", channel, self.client_addr);
        self.pty_writers.lock().await.remove(&channel);
        Ok(())
    }

    async fn channel_eof(
        &mut self,
        channel: ChannelId,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        debug!("Channel EOF: {:?} from {}", channel, self.client_addr);
        Ok(())
    }

    // =========================================================================
    // SECURITY: Whitelist-based request handling
    //
    // ALLOWED (implemented above):
    //   - pty_request: Terminal allocation
    //   - shell_request: Spawn TUI (once per session)
    //   - window_change_request: Terminal resize
    //   - channel_open_session: Session channel
    //   - data: stdin to PTY
    //   - channel_close, channel_eof: Cleanup
    //
    // EXPLICITLY REJECTED (below):
    //   Everything else is explicitly rejected with logging.
    //   This ensures new russh features don't accidentally get allowed.
    // =========================================================================

    async fn exec_request(
        &mut self,
        _channel: ChannelId,
        data: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        let cmd = String::from_utf8_lossy(data);
        error!(
            "SECURITY: exec request from {}: {:?} - disconnecting client",
            self.client_addr,
            cmd.chars().take(100).collect::<String>()
        );
        // Disconnect the client immediately
        session.disconnect(Disconnect::ByApplication, "exec not permitted", "en")?;
        Ok(())
    }

    async fn subsystem_request(
        &mut self,
        _channel: ChannelId,
        name: &str,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        error!(
            "SECURITY: subsystem request '{}' from {} - disconnecting",
            name, self.client_addr
        );
        session.disconnect(Disconnect::ByApplication, "subsystem not permitted", "en")?;
        Ok(())
    }

    async fn env_request(
        &mut self,
        _channel: ChannelId,
        variable_name: &str,
        variable_value: &str,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        // Env requests are commonly sent by SSH clients (TERM, LANG, etc.)
        // Just ignore them - don't even send failure response as it can cause issues
        debug!(
            "Ignoring env request {}={} from {}",
            variable_name,
            variable_value.chars().take(50).collect::<String>(),
            self.client_addr
        );
        // Note: Not sending channel_failure - just silently ignore
        Ok(())
    }

    async fn x11_request(
        &mut self,
        _channel: ChannelId,
        _single_connection: bool,
        _x11_auth_protocol: &str,
        _x11_auth_cookie: &str,
        _x11_screen_number: u32,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        // X11 forwarding often enabled by default in client configs - just ignore
        debug!("Ignoring X11 forwarding request from {}", self.client_addr);
        Ok(())
    }

    async fn signal(
        &mut self,
        _channel: ChannelId,
        signal: russh::Sig,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        // Signals can be legitimate (e.g., window resize sends SIGWINCH)
        debug!("Ignoring signal {:?} from {}", signal, self.client_addr);
        Ok(())
    }

    async fn tcpip_forward(
        &mut self,
        address: &str,
        port: &mut u32,
        _session: &mut Session,
    ) -> Result<bool, Self::Error> {
        // Port forwarding might be in client config - just deny, don't disconnect
        debug!(
            "Denying tcpip-forward request to {}:{} from {}",
            address, port, self.client_addr
        );
        Ok(false)
    }

    async fn cancel_tcpip_forward(
        &mut self,
        address: &str,
        port: u32,
        _session: &mut Session,
    ) -> Result<bool, Self::Error> {
        debug!(
            "Denying cancel-tcpip-forward request for {}:{} from {}",
            address, port, self.client_addr
        );
        Ok(false)
    }

    async fn channel_open_direct_tcpip(
        &mut self,
        channel: Channel<Msg>,
        host_to_connect: &str,
        port_to_connect: u32,
        originator_address: &str,
        originator_port: u32,
        _session: &mut Session,
    ) -> Result<bool, Self::Error> {
        // Active port forwarding attempt - deny but don't disconnect
        warn!(
            "Denying direct-tcpip channel from {} to {}:{} (originator {}:{})",
            self.client_addr,
            host_to_connect,
            port_to_connect,
            originator_address,
            originator_port
        );
        drop(channel);
        Ok(false)
    }

    async fn channel_open_forwarded_tcpip(
        &mut self,
        channel: Channel<Msg>,
        host_to_connect: &str,
        port_to_connect: u32,
        originator_address: &str,
        originator_port: u32,
        _session: &mut Session,
    ) -> Result<bool, Self::Error> {
        warn!(
            "Denying forwarded-tcpip channel from {} to {}:{} (originator {}:{})",
            self.client_addr,
            host_to_connect,
            port_to_connect,
            originator_address,
            originator_port
        );
        drop(channel);
        Ok(false)
    }

    async fn channel_open_direct_streamlocal(
        &mut self,
        channel: Channel<Msg>,
        socket_path: &str,
        _session: &mut Session,
    ) -> Result<bool, Self::Error> {
        warn!(
            "Denying direct-streamlocal channel from {} to socket {}",
            self.client_addr, socket_path
        );
        drop(channel);
        Ok(false)
    }

    async fn streamlocal_forward(
        &mut self,
        socket_path: &str,
        _session: &mut Session,
    ) -> Result<bool, Self::Error> {
        debug!(
            "Denying streamlocal-forward request for {} from {}",
            socket_path, self.client_addr
        );
        Ok(false)
    }

    async fn cancel_streamlocal_forward(
        &mut self,
        socket_path: &str,
        _session: &mut Session,
    ) -> Result<bool, Self::Error> {
        debug!(
            "Denying cancel-streamlocal-forward for {} from {}",
            socket_path, self.client_addr
        );
        Ok(false)
    }

    async fn agent_request(
        &mut self,
        _channel: ChannelId,
        _session: &mut Session,
    ) -> Result<bool, Self::Error> {
        // Agent forwarding often enabled by default - just deny, don't disconnect
        debug!("Denying agent forwarding request from {}", self.client_addr);
        Ok(false)
    }
}
