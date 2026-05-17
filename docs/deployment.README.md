# Deployment README

## Overview

This backend is intended to run as long-running Linux services on a VPS.

The recommended setup is:

- build the Rust binaries on the VPS
- configure runtime values in `.env`
- run the TCP server with `systemd`
- run the Telegram bot with a second `systemd` service
- run the HTTP API with a third `systemd` service
- run subscription maintenance with a daily `systemd` timer
- redeploy by pulling code, rebuilding, and restarting the service

`systemd` is the process manager that keeps each service alive.
tcp sysmtemd config: `/etc/systemd/system/gt06n.service`
telegram bot systemd config: `/etc/systemd/system/gt06n-telegram-bot.service`
http api systemd config: `/etc/systemd/system/gt06n-http-api.service`
subscription maintenance systemd config: `/etc/systemd/system/gt06n-subscription-maintenance.service`
subscription maintenance timer config: `/etc/systemd/system/gt06n-subscription-maintenance.timer`
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
sudo systemctl restart gt06n-http-api.service
sudo systemctl start gt06n-subscription-maintenance.service
sudo systemctl status gt06n.service
sudo systemctl status gt06n-telegram-bot.service
sudo systemctl status gt06n-http-api.service
sudo systemctl status gt06n-subscription-maintenance.service
sudo journalctl -u gt06n.service -f
sudo journalctl -u gt06n-telegram-bot.service -f
sudo journalctl -u gt06n-http-api.service -f
sudo journalctl -u gt06n-subscription-maintenance.service -f
```

The services run database migrations automatically on startup. After deploying code that adds a migration, restart at least one database-backed service before relying on the new feature. In the normal deploy process above, restarting `gt06n.service`, `gt06n-telegram-bot.service`, or `gt06n-http-api.service` is enough to apply migrations.

The subscription maintenance job is not a long-running service. It exits after one pass. Use `systemctl start gt06n-subscription-maintenance.service` to run it immediately, and use the timer below to run it daily.

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

The HTTP API should also have its own unit:

```bash
/etc/systemd/system/gt06n-http-api.service
```

Subscription maintenance should use a one-shot service plus a timer:

```bash
/etc/systemd/system/gt06n-subscription-maintenance.service
/etc/systemd/system/gt06n-subscription-maintenance.timer
```

## Example systemd Units

TCP server:

```ini
[Unit]
Description=GT06N TCP Server
After=network.target

[Service]
Type=simple
WorkingDirectory=/root/gt06n-tcp-server
ExecStart=/root/gt06n-tcp-server/target/release/gt06n-tcp-server
Restart=always
RestartSec=5
EnvironmentFile=/root/gt06n-tcp-server/.env

[Install]
WantedBy=multi-user.target
```

Telegram bot:

```ini
[Unit]
Description=GT06N Telegram Bot
After=network.target

[Service]
Type=simple
WorkingDirectory=/root/gt06n-tcp-server
ExecStart=/root/gt06n-tcp-server/target/release/telegram_bot
Restart=always
RestartSec=5
EnvironmentFile=/root/gt06n-tcp-server/.env

[Install]
WantedBy=multi-user.target
```

HTTP API:

```ini
[Unit]
Description=GT06N HTTP API
After=network.target

[Service]
Type=simple
WorkingDirectory=/root/gt06n-tcp-server
ExecStart=/root/gt06n-tcp-server/target/release/http_api
Restart=always
RestartSec=5
EnvironmentFile=/root/gt06n-tcp-server/.env

[Install]
WantedBy=multi-user.target
```

Subscription maintenance service:

```ini
[Unit]
Description=GT06N Subscription Maintenance
After=network.target

[Service]
Type=oneshot
WorkingDirectory=/root/gt06n-tcp-server
ExecStart=/root/gt06n-tcp-server/target/release/subscription_maintenance
EnvironmentFile=/root/gt06n-tcp-server/.env
```

Subscription maintenance timer:

```ini
[Unit]
Description=Run GT06N Subscription Maintenance Daily

[Timer]
OnCalendar=*-*-* 08:00:00
Persistent=true
Unit=gt06n-subscription-maintenance.service

[Install]
WantedBy=timers.target
```

The application calculates reminder and sanction days using WIB internally. The timer can run at any stable daily time, but `08:00` server time is a practical default. If your VPS timezone is UTC and you want this to run at 08:00 WIB, use `OnCalendar=*-*-* 01:00:00` instead.

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

If you run the HTTP API as a third service, use:

```bash
sudo systemctl restart gt06n-http-api.service
sudo systemctl status gt06n-http-api.service
sudo journalctl -u gt06n-http-api.service -f
```

Subscription maintenance commands:

```bash
sudo systemctl start gt06n-subscription-maintenance.service
sudo systemctl status gt06n-subscription-maintenance.service
sudo journalctl -u gt06n-subscription-maintenance.service -n 100
```

Enable the daily timer:

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now gt06n-subscription-maintenance.timer
sudo systemctl list-timers gt06n-subscription-maintenance.timer
```

Check timer logs:

```bash
sudo journalctl -u gt06n-subscription-maintenance.service -f
```

## Adding Subscription Maintenance In Production

After pulling the version that includes subscription maintenance:

```bash
cd /root/gt06n-tcp-server
git pull
cargo build --release
sudo systemctl restart gt06n.service
sudo systemctl restart gt06n-telegram-bot.service
sudo systemctl restart gt06n-http-api.service
```

Create the service file:

```bash
sudo nano /etc/systemd/system/gt06n-subscription-maintenance.service
```

Paste the `GT06N Subscription Maintenance` service unit from above.

Create the timer file:

```bash
sudo nano /etc/systemd/system/gt06n-subscription-maintenance.timer
```

Paste the `Run GT06N Subscription Maintenance Daily` timer unit from above.

Load systemd, test one manual run, then enable the timer:

```bash
sudo systemctl daemon-reload
sudo systemctl start gt06n-subscription-maintenance.service
sudo systemctl status gt06n-subscription-maintenance.service
sudo journalctl -u gt06n-subscription-maintenance.service -n 100
sudo systemctl enable --now gt06n-subscription-maintenance.timer
sudo systemctl list-timers gt06n-subscription-maintenance.timer
```

The maintenance job needs the same `.env` values as the bot:

- `DATABASE_URL`
- `TELEGRAM_BOT_TOKEN`

It sends the D-5 subscription reminder, daily late-payment reminders for days 1-7, updates sanction state, and marks withdrawal-required after day 7.
