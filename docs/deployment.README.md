# VPS Deployment Guide

This guide explains how to deploy the GT06 / Concox TCP server to an Ubuntu VPS, verify that it is reachable, and run it safely with `systemd`.

## Current Assumptions

- The repository is already cloned on the VPS
- Current repo path on the VPS: `/root/gt06n-tcp-server`
- Rust is already installed on the VPS
- You want to build on the Ubuntu server, not cross-compile from Windows

## 1. Build The Release Binary

SSH into the VPS and run:

```bash
cd /root/gt06n-tcp-server
cargo build --release
```

This creates the production binary at:

```bash
/root/gt06n-tcp-server/target/release/gt06n-tcp-server
```

## 2. Configure Environment Variables

Create the runtime `.env` file:

```bash
cd /root/gt06n-tcp-server
cp .env.example .env
nano .env
```

Example:

```env
GT06_BIND_ADDR=0.0.0.0:5000
GT06_READ_BUFFER_CAPACITY=4096
RUST_LOG=info
DATABASE_URL=postgres://postgres:postgres@localhost:5432/gt06n_tcp_server
DATABASE_MAX_CONNECTIONS=5
DATABASE_WRITE_TIMEOUT_MS=5000
```

### Important Setting

- `GT06_BIND_ADDR=0.0.0.0:5000`
  - listens on TCP port `5000` on all VPS network interfaces
- if you only use `127.0.0.1:5000`
  - the server will only accept local connections inside the VPS

## 3. Test Run The Binary Manually

Before setting up `systemd`, run the server directly once:

```bash
cd /root/gt06n-tcp-server
./target/release/gt06n-tcp-server
```

Expected startup log:

```text
GT06N TCP server started bind_addr=0.0.0.0:5000
```

If PostgreSQL is configured, startup will also run database migrations automatically before the server begins accepting tracker connections.

## 4. Confirm The Server Is Listening

From another VPS terminal:

```bash
ss -ltn | grep 5000
```

Expected output is similar to:

```text
LISTEN 0 128 0.0.0.0:5000 0.0.0.0:*
```

That means the process is listening on port `5000`.

## 5. Confirm The Port Is Reachable From Outside

From your local machine:

### Windows PowerShell

```powershell
Test-NetConnection -ComputerName YOUR_VPS_IP -Port 5000
```

Expected result:

```text
TcpTestSucceeded : True
```

### Git Bash On Windows

If you are using Git Bash, call PowerShell explicitly:

```bash
powershell.exe -Command "Test-NetConnection -ComputerName YOUR_VPS_IP -Port 5000"
```

### What This Proves

If the remote check succeeds, then:

- the Rust app is running
- the VPS is listening on the configured port
- the firewall and network path allow inbound TCP traffic

## 6. Open The Firewall

If the VPS uses `ufw`:

```bash
sudo ufw allow 5000/tcp
sudo ufw status
```

If your cloud provider also has a firewall or security group, make sure TCP port `5000` is allowed there too.

## 7. Run The Server With systemd

Create the service file:

```bash
sudo nano /etc/systemd/system/gt06n.service
```

Put this inside:

```ini
[Unit]
Description=GT06N TCP Server
After=network.target

[Service]
Type=simple
WorkingDirectory=/root/gt06n-tcp-server
EnvironmentFile=/root/gt06n-tcp-server/.env
ExecStart=/root/gt06n-tcp-server/target/release/gt06n-tcp-server
Restart=on-failure
RestartSec=5

[Install]
WantedBy=multi-user.target
```

Then reload and start:

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now gt06n.service
```

Check the service status:

```bash
sudo systemctl status gt06n.service
```

Expected state:

```text
active (running)
```

## 8. View Logs

To watch logs in real time:

```bash
sudo journalctl -u gt06n.service -f
```

This is where you should see:

- server startup log
- device connections
- login acceptance
- heartbeat packets
- location packets

With PostgreSQL enabled, these tracker events are also persisted into:

- `devices`
- `device_locations`
- `device_heartbeats`

## 9. Common systemd Commands

Restart the service:

```bash
sudo systemctl restart gt06n.service
```

Stop the service:

```bash
sudo systemctl stop gt06n.service
```

Start the service:

```bash
sudo systemctl start gt06n.service
```

Disable auto-start on boot:

```bash
sudo systemctl disable gt06n.service
```

## 10. Update The App After New Code Changes

When you push new code and want to redeploy:

```bash
cd /root/gt06n-tcp-server
git pull
cargo build --release
sudo systemctl restart gt06n.service
```

Then verify:

```bash
sudo systemctl status gt06n.service
sudo journalctl -u gt06n.service -n 50
```

## 11. Production Notes

The current setup works and is good for getting the service online quickly, but it is not the final hardened layout yet because it runs from `/root`.

For a safer long-term production setup, the next improvement should be:

- create a dedicated Linux user such as `gt06n`
- move the application to `/opt/gt06n` or `/srv/gt06n`
- run the `systemd` service as that dedicated user

## 12. What Success Looks Like

You know deployment is working when all of these are true:

- `cargo build --release` succeeds
- manual binary startup works
- `ss -ltn | grep 5000` shows the service listening
- `Test-NetConnection` from your local machine reports `TcpTestSucceeded : True`
- `systemctl status gt06n.service` shows `active (running)`
- `journalctl -u gt06n.service -f` shows GT06 / Concox device traffic after the tracker is configured
- PostgreSQL migrations complete successfully when `DATABASE_URL` is set
