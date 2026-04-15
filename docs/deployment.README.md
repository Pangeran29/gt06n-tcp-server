# Deployment README

## Overview

This backend is intended to run as long-running Linux services on a VPS.

The recommended setup is:

- build the Rust binaries on the VPS
- configure runtime values in `.env`
- run the TCP server with `systemd`
- optionally run the Telegram bot with a second `systemd` service
- redeploy by pulling code, rebuilding, and restarting the service

`systemd` is the process manager that keeps each service alive.
tcp sysmtemd config: `/etc/systemd/system/gt06n.service`
telegram bot systemd config: `/etc/systemd/system/gt06n-telegram-bot.service`
It is responsible for:

- starting the service at boot
- restarting it if it crashes
- giving you a standard way to inspect logs and status

## Deploy Process

When you push new code and want to redeploy:

```bash
cd /root/gt06n-tcp-server
git pull
cargo build --release
sudo systemctl restart gt06n.service
sudo systemctl restart gt06n-telegram-bot.service
sudo systemctl status gt06n.service
sudo systemctl status gt06n-telegram-bot.service
sudo journalctl -u gt06n.service -f
sudo journalctl -u gt06n-telegram-bot.service -f
```

## systemd Service

The TCP backend is managed by the `gt06n.service` service unit.

Typical service file location:

```bash
/etc/systemd/system/gt06n.service
```

If you also deploy the Telegram bot as a service, it should have its own unit, for example:

```bash
/etc/systemd/system/gt06n-telegram-bot.service
```

## Essential systemd Commands

Start the TCP backend:

```bash
sudo systemctl start gt06n.service
```

Stop the TCP backend:

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

If you run the Telegram bot as a second service, the commands are the same pattern, for example:

```bash
sudo systemctl restart gt06n-telegram-bot.service
sudo systemctl status gt06n-telegram-bot.service
sudo journalctl -u gt06n-telegram-bot.service -f
```


