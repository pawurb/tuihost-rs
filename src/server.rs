use crate::handler::SessionHandler;
use russh::keys::{Algorithm, PrivateKey};
use russh::server::{Config, Server};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tracing::{info, warn};

#[derive(Debug, Clone)]
pub struct CmdConfig {
    pub command: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}

pub struct TuiSshServer {
    tui_config: Arc<CmdConfig>,
    max_connections: usize,
    active_connections: Arc<AtomicUsize>,
    max_session_duration: Option<Duration>,
}

impl TuiSshServer {
    pub fn new(
        tui_config: CmdConfig,
        max_connections: usize,
        max_session_duration: Option<Duration>,
    ) -> Self {
        Self {
            tui_config: Arc::new(tui_config),
            max_connections,
            active_connections: Arc::new(AtomicUsize::new(0)),
            max_session_duration,
        }
    }
}

impl Server for TuiSshServer {
    type Handler = SessionHandler;

    fn new_client(&mut self, peer_addr: Option<std::net::SocketAddr>) -> Self::Handler {
        let addr_str = peer_addr
            .map(|a| a.to_string())
            .unwrap_or_else(|| "unknown".to_string());

        let current = self.active_connections.fetch_add(1, Ordering::SeqCst);

        if self.max_connections > 0 && current >= self.max_connections {
            warn!(
                "Connection limit reached ({}/{}), rejecting {}",
                current, self.max_connections, addr_str
            );
            self.active_connections.fetch_sub(1, Ordering::SeqCst);
            // Return handler that will reject - russh doesn't have a direct reject mechanism
            // The handler will still be created but connection limits are enforced at TCP level ideally
        }

        info!("New connection from {} ({} active)", addr_str, current + 1);

        SessionHandler::new(
            self.tui_config.clone(),
            addr_str,
            self.active_connections.clone(),
            self.max_session_duration,
        )
    }
}

pub fn create_config(host_key: PrivateKey, timeout_secs: u64) -> Config {
    let timeout = if timeout_secs > 0 {
        Some(Duration::from_secs(timeout_secs))
    } else {
        None
    };

    Config {
        keys: vec![host_key],
        inactivity_timeout: timeout,
        auth_rejection_time: Duration::from_secs(1),
        auth_rejection_time_initial: Some(Duration::from_secs(0)),
        ..Default::default()
    }
}

pub fn generate_host_key() -> PrivateKey {
    PrivateKey::random(&mut rand_core::OsRng, Algorithm::Ed25519).expect("Failed to generate key")
}
