use crate::handler::SessionHandler;
use russh::keys::{Algorithm, PrivateKey};
use russh::server::{Config, Server};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::sync::Semaphore;
use tracing::info;

#[derive(Debug, Clone)]
pub struct CmdConfig {
    pub command: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}

pub struct TuiSshServer {
    tui_config: Arc<CmdConfig>,
    /// Caps concurrently running commands (`None` = unlimited). A permit is
    /// taken right before a command is spawned and released when its session
    /// task ends, so idle or never-authenticated connections don't hold slots.
    session_limit: Option<Arc<Semaphore>>,
    /// Open TCP connections, for logging only. Incremented in `new_client`,
    /// decremented once in `SessionHandler::drop`.
    active_connections: Arc<AtomicUsize>,
    max_session_duration: Option<Duration>,
}

impl TuiSshServer {
    pub fn new(
        tui_config: CmdConfig,
        max_sessions: usize,
        max_session_duration: Option<Duration>,
    ) -> Self {
        Self {
            tui_config: Arc::new(tui_config),
            session_limit: (max_sessions > 0).then(|| Arc::new(Semaphore::new(max_sessions))),
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
        info!("New connection from {} ({} active)", addr_str, current + 1);

        SessionHandler::new(
            self.tui_config.clone(),
            addr_str,
            self.active_connections.clone(),
            self.session_limit.clone(),
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
    PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).expect("Failed to generate key")
}
