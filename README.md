# tuihost
[![Latest Version](https://img.shields.io/crates/v/tuihost.svg)](https://crates.io/crates/tuihost)

> ⚠️ Disclaimer  
> Mostly one-shotted with Claude Code. Not audited. You've been warned. 

Inspired by [terminal.shop](https://terminal.shop). Created to host the demo TUI for [hotpath.rs](https://hotpath.rs). 

It enables ssh login via just `ssh your-host.com` instead of `ssh demo@your-host.com`. Unlike OpenSSH, `tuihost` implements its own SSH server and ignores the username field entirely. Any incoming SSH connection is accepted and mapped directly to a forced bash command session. 

## Features

- Zero authentication friction - just `ssh your-host.com` and you're in
- Allocate PTY and spawn configurable TUI command
- Bidirectional I/O between SSH channel and PTY
- Terminal resize support
- Auto-generates Ed25519 host key if not present
- Connection limits and timeouts

## Installation

```bash
cargo install tuihost
```

## Usage

```bash
# Basic usage
tuihost -c htop

# Custom port and command with args
tuihost -l 0.0.0.0:22 -c /usr/bin/vim -a -R /etc/motd

# With connection limits
tuihost -c top --max-connections 50 --timeout 600
```

## Options

```
-l, --listen <ADDR>          Address to listen on [default: 0.0.0.0:2222]
-k, --host-key <PATH>        Path to SSH host key [default: ./host_key]
-c, --command <CMD>          Command to execute for each connection
-a, --args <ARGS>...         Arguments to pass to the command
-e, --env <KEY=VALUE>        Environment variables to pass (clean env by default)
    --max-connections <N>    Max concurrent connections [default: 100]
    --timeout <SECS>         Session timeout in seconds [default: 300]
```

## Examples

```bash
# Run htop for monitoring
tuihost -c htop

# Interactive shell
tuihost -c /bin/bash -a -l

# Command with multiple args (everything after -a is passed to the command)
tuihost -c vim -a -R /etc/hosts

# Pass environment variables (child process starts with clean env)
tuihost -c myapp -e TERM=xterm-256color -e DATABASE_URL=postgres://localhost/db

# Production settings
tuihost -l 0.0.0.0:22 -k /etc/tuihost/host_key -c myapp --max-connections 200 --timeout 3600
```

## Security

### Built-in protections
- PTY size validation (prevents resource exhaustion)
- Connection limits
- Session timeouts
- Auth rejection delay (slows brute force)

### Running as a dedicated user

Spawned commands inherit the same user permissions as the tuihost process. **Never run tuihost as root** - if your TUI application has a vulnerability, attackers could gain full system access.

Create a restricted system user:

```bash
# Create system user with no home directory and no login shell
sudo useradd --system --no-create-home --shell /usr/sbin/nologin tuihost

# Create directory for host key
sudo mkdir -p /etc/tuihost
sudo chown tuihost:tuihost /etc/tuihost
sudo chmod 700 /etc/tuihost
```

Example systemd service with sandboxing (`/etc/systemd/system/tuihost.service`):

```ini
[Unit]
Description=tuihost SSH TUI server
After=network.target

[Service]
Type=simple
User=tuihost
Group=tuihost
ExecStart=/usr/local/bin/tuihost -l 0.0.0.0:2222 -k /etc/tuihost/host_key -c /usr/bin/htop

# Sandboxing
NoNewPrivileges=yes
ProtectSystem=strict
ProtectHome=yes
PrivateTmp=yes
PrivateDevices=yes
ProtectKernelTunables=yes
ProtectControlGroups=yes
RestrictNamespaces=yes
RestrictRealtime=yes
RestrictSUIDSGID=yes

# Allow binding to privileged port 22 if needed
AmbientCapabilities=CAP_NET_BIND_SERVICE
CapabilityBoundingSet=CAP_NET_BIND_SERVICE

[Install]
WantedBy=multi-user.target
```

Enable and start:

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now tuihost
```

### iptables rate limiting

```bash
# Rate limit new connections (10/min per IP)
iptables -A INPUT -p tcp --dport 2222 -m state --state NEW -m recent --set
iptables -A INPUT -p tcp --dport 2222 -m state --state NEW -m recent --update --seconds 60 --hitcount 10 -j DROP

# Limit concurrent connections per IP
iptables -A INPUT -p tcp --dport 2222 -m connlimit --connlimit-above 5 -j REJECT
```

### fail2ban integration

Create `/etc/fail2ban/filter.d/tuihost.conf`:
```ini
[Definition]
failregex = New connection from <HOST>
ignoreregex =
```

Create `/etc/fail2ban/jail.d/tuihost.conf`:
```ini
[tuihost]
enabled = true
port = 2222
filter = tuihost
logpath = /var/log/tuihost.log
maxretry = 10
findtime = 60
bantime = 3600
```

## Connect

Just SSH. That's it. No username, no password, no SSH keys to configure.

```bash
ssh demo.hotpath.rs
```

Your users are one command away from your app.

## License

MIT
