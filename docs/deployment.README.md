# Deployment README

## Overview

This backend is intended to run as a long-running Linux service on a VPS.

The recommended setup is:

- build the Rust binary on the VPS
- configure runtime values in `.env`
- run the backend with `systemd`
- redeploy by pulling code, rebuilding, and restarting the service

`systemd` is the process manager that keeps the backend alive.

It is responsible for:

- starting the backend at boot
- restarting it if it crashes
- giving you a standard way to inspect logs and status

## Deploy Process

When you push new code and want to redeploy:

```bash
cd /root/gt06n-tcp-server
git pull
cargo build --release
sudo systemctl restart gt06n.service
sudo systemctl status gt06n.service
sudo journalctl -u gt06n.service -f
```

## systemd Service

The backend is managed by the `gt06n.service` service unit.

Typical service file location:

```bash
/etc/systemd/system/gt06n.service
```

## Essential systemd Commands

Start the backend:

```bash
sudo systemctl start gt06n.service
```

Stop the backend:

```bash
sudo systemctl stop gt06n.service
```

Restart the backend:

```bash
sudo systemctl restart gt06n.service
```

Check current status:

```bash
sudo systemctl status gt06n.service
```

Enable auto-start on boot:

```bash
sudo systemctl enable gt06n.service
```

Watch backend logs:

```bash
sudo journalctl -u gt06n.service -f
```
