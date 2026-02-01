use crate::pty::{PtySession, PtyWriter};
use crate::server::CmdConfig;
use russh::server::{Auth, Handler, Msg, Session};
use russh::{Channel, ChannelId, CryptoVec};
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
}
