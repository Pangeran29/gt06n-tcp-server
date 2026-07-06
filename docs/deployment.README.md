# Deployment

This project is designed to run on a Linux VPS with `systemd`.

## Processes

Production uses four processes:

| Component | Binary | Unit |
| --- | --- | --- |
| TCP server | `gt06n-tcp-server` | `gt06n.service` |
| Telegram bot | `telegram_bot` | `gt06n-telegram-bot.service` |
| HTTP API | `http_api` | `gt06n-http-api.service` |
| Subscription maintenance | `subscription_maintenance` | `gt06n-subscription-maintenance.service` + timer |

All services read the same project `.env`.

## Normal Deploy

```bash
cd /root/gt06n-tcp-server
git pull
cargo build --release

sudo systemctl restart gt06n.service
sudo systemctl restart gt06n-telegram-bot.service
sudo systemctl restart gt06n-http-api.service
```

If you want the reminder/sanction job to run immediately after deploy:

```bash
sudo systemctl start gt06n-subscription-maintenance.service
```

## Checks After Deploy

Service status:

```bash
sudo systemctl status gt06n.service
sudo systemctl status gt06n-telegram-bot.service
sudo systemctl status gt06n-http-api.service
sudo systemctl status gt06n-subscription-maintenance.timer
```

Logs:

```bash
sudo journalctl -u gt06n.service -f
sudo journalctl -u gt06n-telegram-bot.service -f
sudo journalctl -u gt06n-http-api.service -f
sudo journalctl -u gt06n-subscription-maintenance.service -f
```

## First-Time Setup

Create these unit files:

- `/etc/systemd/system/gt06n.service`
- `/etc/systemd/system/gt06n-telegram-bot.service`
- `/etc/systemd/system/gt06n-http-api.service`
- `/etc/systemd/system/gt06n-subscription-maintenance.service`
- `/etc/systemd/system/gt06n-subscription-maintenance.timer`

Then:

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now gt06n.service
sudo systemctl enable --now gt06n-telegram-bot.service
sudo systemctl enable --now gt06n-http-api.service
sudo systemctl enable --now gt06n-subscription-maintenance.timer
```

## Unit File Examples

### `gt06n.service`

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

### `gt06n-telegram-bot.service`

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

### `gt06n-http-api.service`

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

### `gt06n-subscription-maintenance.service`

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

### `gt06n-subscription-maintenance.timer`

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

If the VPS timezone is UTC and you want the job to run at 08:00 WIB, set:

```ini
OnCalendar=*-*-* 01:00:00
```

## Subscription Maintenance Notes

- this binary is `Type=oneshot`
- after success, `inactive (dead)` is normal
- success should show `Deactivated successfully.`

## Operational Notes

- database migrations run automatically when a database-backed binary starts
- after payment-related schema changes, restart both `telegram_bot` and `http_api`
- after reminder/sanction changes, restart `subscription_maintenance` or run it once manually
