# Deployment README

## Overview

This backend is intended to run on a Linux VPS with `systemd`.

Production runs four processes:

| Component | Binary | systemd unit | Purpose |
| --- | --- | --- | --- |
| TCP server | `gt06n-tcp-server` | `gt06n.service` | Receives GT06 GPS tracker packets |
| Telegram bot | `telegram_bot` | `gt06n-telegram-bot.service` | Handles Telegram commands, alerts, and payments |
| HTTP API | `http_api` | `gt06n-http-api.service` | Serves location history and Midtrans webhook |
| Subscription maintenance | `subscription_maintenance` | `gt06n-subscription-maintenance.timer` | Runs daily reminders, sanctions, and withdrawal flags |

All services use:

```bash
WorkingDirectory=/root/gt06n-tcp-server
EnvironmentFile=/root/gt06n-tcp-server/.env
```

Database migrations run automatically when a database-backed binary starts.

## Normal Deploy

Use this when code changes are pushed and the systemd unit files did not change.

```bash
cd /root/gt06n-tcp-server
git pull
cargo build --release

sudo systemctl restart gt06n.service
sudo systemctl restart gt06n-telegram-bot.service
sudo systemctl restart gt06n-http-api.service
```

Optional: run subscription maintenance immediately after deploy instead of waiting for the daily timer.

```bash
sudo systemctl start gt06n-subscription-maintenance.service
```

Check status:

```bash
sudo systemctl status gt06n.service
sudo systemctl status gt06n-telegram-bot.service
sudo systemctl status gt06n-http-api.service
sudo systemctl status gt06n-subscription-maintenance.timer
```

Tail logs:

```bash
sudo journalctl -u gt06n.service -f
sudo journalctl -u gt06n-telegram-bot.service -f
sudo journalctl -u gt06n-http-api.service -f
sudo journalctl -u gt06n-subscription-maintenance.service -f
```

## First-Time Service Setup

Create or update these systemd files:

```bash
/etc/systemd/system/gt06n.service
/etc/systemd/system/gt06n-telegram-bot.service
/etc/systemd/system/gt06n-http-api.service
/etc/systemd/system/gt06n-subscription-maintenance.service
/etc/systemd/system/gt06n-subscription-maintenance.timer
```

After creating or editing systemd unit files:

```bash
sudo systemctl daemon-reload

sudo systemctl enable --now gt06n.service
sudo systemctl enable --now gt06n-telegram-bot.service
sudo systemctl enable --now gt06n-http-api.service
sudo systemctl enable --now gt06n-subscription-maintenance.timer
```

Run the subscription maintenance job once manually to verify it works:

```bash
sudo systemctl start gt06n-subscription-maintenance.service
sudo systemctl status gt06n-subscription-maintenance.service
sudo journalctl -u gt06n-subscription-maintenance.service -n 100
```

`gt06n-subscription-maintenance.service` is a `Type=oneshot` service. After a successful run, it is normal for status to show:

```text
Active: inactive (dead)
```

The success signal is:

```text
Deactivated successfully.
```

## systemd Unit Files

### TCP Server

Path:

```bash
/etc/systemd/system/gt06n.service
```

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

### Telegram Bot

Path:

```bash
/etc/systemd/system/gt06n-telegram-bot.service
```

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

### HTTP API

Path:

```bash
/etc/systemd/system/gt06n-http-api.service
```

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

### Subscription Maintenance

Path:

```bash
/etc/systemd/system/gt06n-subscription-maintenance.service
```

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

Path:

```bash
/etc/systemd/system/gt06n-subscription-maintenance.timer
```

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

The application calculates reminder and sanction days using WIB internally. If your VPS timezone is UTC and you want the timer to run at 08:00 WIB, use:

```ini
OnCalendar=*-*-* 01:00:00
```

## Useful Commands

Restart services:

```bash
sudo systemctl restart gt06n.service
sudo systemctl restart gt06n-telegram-bot.service
sudo systemctl restart gt06n-http-api.service
```

Run subscription maintenance now:

```bash
sudo systemctl start gt06n-subscription-maintenance.service
```

Check timers:

```bash
sudo systemctl status gt06n-subscription-maintenance.timer
sudo systemctl list-timers gt06n-subscription-maintenance.timer
```

Enable services on boot:

```bash
sudo systemctl enable gt06n.service
sudo systemctl enable gt06n-telegram-bot.service
sudo systemctl enable gt06n-http-api.service
sudo systemctl enable --now gt06n-subscription-maintenance.timer
```

Stop services:

```bash
sudo systemctl stop gt06n.service
sudo systemctl stop gt06n-telegram-bot.service
sudo systemctl stop gt06n-http-api.service
sudo systemctl stop gt06n-subscription-maintenance.timer
```

## Subscription Maintenance Notes

The maintenance job needs these `.env` values:

- `DATABASE_URL`
- `TELEGRAM_BOT_TOKEN`

It runs once per timer event and then exits.

It performs:

- D-5 subscription expiry reminder
- daily late-payment reminders for overdue days 1-7
- Rp 1.000/day sanction calculation, capped at Rp 7.000
- withdrawal-required flag after day 7

No admin Telegram alert is sent for withdrawal-required users in the current version. The team handles device withdrawal manually.

## Troubleshooting

If a binary was updated but service behavior did not change:

```bash
cargo build --release
sudo systemctl restart gt06n-telegram-bot.service
```

If a systemd unit file was edited:

```bash
sudo systemctl daemon-reload
sudo systemctl restart gt06n.service
sudo systemctl restart gt06n-telegram-bot.service
sudo systemctl restart gt06n-http-api.service
sudo systemctl restart gt06n-subscription-maintenance.timer
```

If subscription maintenance shows `inactive (dead)` after a manual run, that is expected for a successful one-shot service. Check logs for errors:

```bash
sudo journalctl -u gt06n-subscription-maintenance.service -n 100
```
