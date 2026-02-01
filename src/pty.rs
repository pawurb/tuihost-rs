use anyhow::{Context, Result};
use pty_process::Size;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub struct PtySession {
    pty: pty_process::Pty,
    _child: tokio::process::Child,
}

impl PtySession {
    pub fn spawn(
        command: &str,
        args: &[String],
        env: &[(String, String)],
        cols: u16,
        rows: u16,
    ) -> Result<Self> {
        let (pty, pts) = pty_process::open().context("Failed to create PTY")?;

        pty.resize(Size::new(rows, cols))
            .context("Failed to set initial PTY size")?;

        let child = pty_process::Command::new(command)
            .args(args)
            .env_clear()
            .env("TERM", "xterm-256color")
            .env("LANG", "en_US.UTF-8")
            .envs(env.iter().cloned())
            .spawn(pts)
            .with_context(|| format!("Failed to spawn command: {}", command))?;

        Ok(Self { pty, _child: child })
    }

    pub fn split(self) -> (PtyReader, PtyWriter) {
        let (reader, writer) = self.pty.into_split();
        (PtyReader { reader }, PtyWriter { writer })
    }
}

pub struct PtyReader {
    reader: pty_process::OwnedReadPty,
}

impl PtyReader {
    pub async fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.reader.read(buf).await
    }
}

pub struct PtyWriter {
    writer: pty_process::OwnedWritePty,
}

impl PtyWriter {
    pub async fn write_all(&mut self, data: &[u8]) -> std::io::Result<()> {
        self.writer.write_all(data).await
    }

    pub fn resize(&self, cols: u16, rows: u16) -> Result<()> {
        self.writer
            .resize(Size::new(rows, cols))
            .context("Failed to resize PTY")
    }
}
