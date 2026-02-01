mod handler;
mod pty;
mod server;

use anyhow::{Context, Result};
use clap::Parser;
use russh::keys::PrivateKey;
use russh::server::Server as _;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use tokio::net::TcpListener;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

use crate::server::{CmdConfig, TuiSshServer, create_config, generate_host_key};

#[derive(Parser, Debug)]
#[command(name = "tuihost")]
#[command(about = "SSH server that spawns a forced TUI application")]
#[command(version)]
struct Args {
    /// Address to listen on
    #[arg(short, long, default_value = "0.0.0.0:2222")]
    listen: String,

    /// Path to SSH host key (generated if missing)
    #[arg(short = 'k', long, default_value = "./host_key")]
    host_key: String,

    /// Command to execute for each connection
    #[arg(short, long)]
    command: String,

    /// Arguments to pass to the command
    #[arg(short, long, num_args = 0.., allow_hyphen_values = true)]
    args: Vec<String>,

    /// Environment variables to pass to the command (KEY=VALUE)
    #[arg(short, long, value_name = "KEY=VALUE")]
    env: Vec<String>,

    /// Maximum concurrent connections (0 = unlimited)
    #[arg(long, default_value = "100")]
    max_connections: usize,

    /// Session timeout in seconds (0 = no timeout)
    #[arg(long, default_value = "300")]
    timeout: u64,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("tuihost=info".parse()?))
        .init();

    let args = Args::parse();

    info!(
        "Starting tuihost server on {} with command: {} {:?}",
        args.listen, args.command, args.args
    );

    let host_key = load_or_generate_host_key(&args.host_key)?;

    let env_vars: Vec<(String, String)> = args
        .env
        .iter()
        .filter_map(|e| {
            let mut parts = e.splitn(2, '=');
            match (parts.next(), parts.next()) {
                (Some(key), Some(value)) => Some((key.to_string(), value.to_string())),
                _ => {
                    warn!("Invalid env var format (expected KEY=VALUE): {}", e);
                    None
                }
            }
        })
        .collect();

    let tui_config = CmdConfig {
        command: args.command,
        args: args.args,
        env: env_vars,
    };

    let ssh_config = create_config(host_key, args.timeout);
    let mut server = TuiSshServer::new(tui_config, args.max_connections);

    let listener = TcpListener::bind(&args.listen)
        .await
        .with_context(|| format!("Failed to bind to {}", args.listen))?;

    info!("SSH server listening on {}", args.listen);

    server
        .run_on_socket(std::sync::Arc::new(ssh_config), &listener)
        .await?;

    Ok(())
}

fn load_or_generate_host_key(path: &str) -> Result<PrivateKey> {
    let key_path = Path::new(path);

    if key_path.exists() {
        info!("Loading host key from: {}", path);
        let key_data = std::fs::read_to_string(key_path).context("Failed to read host key file")?;
        let key = key_data
            .parse::<PrivateKey>()
            .map_err(|e| anyhow::anyhow!("Failed to parse host key: {}", e))?;
        Ok(key)
    } else {
        warn!(
            "Host key not found, generating new Ed25519 key at: {}",
            path
        );
        let key = generate_host_key();

        let openssh_key = key
            .to_openssh(ssh_key::LineEnding::LF)
            .map_err(|e| anyhow::anyhow!("Failed to encode key: {}", e))?;
        std::fs::write(key_path, openssh_key.as_bytes())
            .context("Failed to write host key file")?;

        // Set secure permissions (600)
        std::fs::set_permissions(key_path, std::fs::Permissions::from_mode(0o600))
            .context("Failed to set host key permissions")?;

        info!("Generated and saved new host key");
        Ok(key)
    }
}
