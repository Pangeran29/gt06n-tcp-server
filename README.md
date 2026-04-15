# GT06 / Concox TCP Server

## Overview Project

This project is a Rust tracking backend for `GT06` / `Concox` GPS trackers.

Today the repo contains two Rust services:

- the TCP server that receives raw tracker packets, decodes them, and stores the data
- the Telegram bot that reads stored heartbeat data from PostgreSQL and sends notifications to one admin chat

The backend is already suitable as the ingestion and first notification layer for a tracking system. It is not yet a complete product with public APIs, dashboards, authentication, or business workflows.

The TCP server currently supports these protocol packets:

- login (`0x01`)
- heartbeat (`0x13`)
- classic location (`0x12`)
- Concox extended location (`0x22`)

## How To Run It

### 1. Configure `.env`

Create a `.env` file in the project root:

```env
GT06_BIND_ADDR=0.0.0.0:5000
GT06_READ_BUFFER_CAPACITY=4096
RUST_LOG=info
DATABASE_URL=postgres://postgres:postgres@localhost:5432/gt06n_tcp_server
DATABASE_MAX_CONNECTIONS=10
DATABASE_WRITE_TIMEOUT_MS=5000
TELEGRAM_BOT_TOKEN=
TELEGRAM_ADMIN_CHAT_ID=
TELEGRAM_POLL_TIMEOUT_SECS=30
TELEGRAM_HEARTBEAT_POLL_INTERVAL_MS=3000
```

Notes:

- if `DATABASE_URL` is not set, the TCP server still runs, but without PostgreSQL persistence
- if `DATABASE_URL` is set, the app connects to PostgreSQL and runs migrations automatically at startup
- the PostgreSQL database itself must already exist before the service starts
- if `TELEGRAM_BOT_TOKEN` is set, you can run the Telegram bot service
- if `TELEGRAM_ADMIN_CHAT_ID` is empty, the bot can be bound from Telegram with `/bind_me`

### 2. Run the TCP Server

```powershell
cargo run
```

For release mode:

```powershell
cargo run --release
```

### 3. Run the Telegram Bot

```powershell
cargo run --bin telegram_bot
```

For release mode:

```powershell
cargo run --release --bin telegram_bot
```

The bot uses long polling and reads heartbeat data from PostgreSQL. It does not receive data directly from the TCP socket layer.

### 4. Check that the TCP server is listening

Windows PowerShell:

```powershell
Test-NetConnection -ComputerName 127.0.0.1 -Port 5000
```

Linux / VPS:

```bash
ss -ltn | grep 5000
```

### 5. Run tests

```powershell
cargo test
```

## What This Has

### TCP Ingestion

- async TCP server using `tokio`
- GT06 `0x7878` frame parsing
- CRC validation
- login ACK support
- heartbeat ACK support
- support for real Concox login packet variants with extra trailing bytes

### Decoded Tracking Data

For location packets:

- GPS timestamp
- latitude
- longitude
- speed
- course
- course status
- satellite count
- GPS info length
- extra packet bytes for extended Concox location packets

For heartbeat packets:

- raw `terminal_info`
- `terminal_info_bits`
- validated `gps_tracking_on`
- validated `acc_high`
- validated `vibration_detected`
- neutral guess fields for remaining heartbeat bits
- voltage level
- GSM signal strength
- alarm/language field
- best-effort `engine_status_guess`

### PostgreSQL Storage

When PostgreSQL is enabled, the server stores data into:

- `devices`
- `device_locations`
- `device_heartbeats`

The schema stores both:

- normalized query-friendly fields
- raw protocol evidence such as `terminal_info`, `terminal_info_bits`, `extra_data_hex`, and protocol number

This is intentional so future parser improvements do not make old data useless.

### Telegram Integration

The repo also includes a separate Telegram bot service that:

- uses Telegram Bot API long polling
- reads new heartbeat rows from PostgreSQL
- sends one notification per new heartbeat row
- supports an initial command surface:
  - `/start`
  - `/help`
  - `/bind_me`
  - `/last_heartbeat`
  - `/latest_location`

The bot stores its own operational state in PostgreSQL so it can continue safely after restart. That state currently includes:

- last processed Telegram update id
- last notified heartbeat id
- bound admin chat id

Important note:

- `engine_status_guess` is still heuristic and should not yet be treated as a fully validated ignition signal
- heartbeat bits now mix validated names and neutral `bit_*_guess` placeholders until the remaining bits are confirmed on the real device

More detail:

- database design: [database.README.md](/d:/jonathan/gps-tracker/tcp-server/docs/database.README.md)
- deployment process: [deployment.README.md](/d:/jonathan/gps-tracker/tcp-server/docs/deployment.README.md)
- telegram bot: [telegram.README.md](/d:/jonathan/gps-tracker/tcp-server/docs/telegram.README.md)

## What Need To Be Implemented

The main remaining work is:

- validate `engine_status_guess` against real ignition on/off tests
- decode packet `0x26`
- support more Concox alarm and status packet variants
- build a read API for latest device state and history
- add dashboard / map UI
- expand Telegram commands beyond heartbeat-first operations
- add delivery rules such as status-change-only notifications and richer filtering
- support production deployment for the Telegram bot with its own `systemd` service
- evaluate Telegram Stars / payment flows later if the product direction needs them
- add retention and archival strategy for long-running GPS history
- improve production hardening, especially running the service under a dedicated Linux user instead of `/root`



motor on, is this you?
a. yes, its me -> okay, enjoy your ride
b. not, me -> there's indication that the motor is being "dicuri" rn use below link to detect your motor

later

how's you riding, you traveled 40km today, your average speed is 40km, you are riding for 40 minutes, open this link to view detail perjalana.
or
youre ridign for 40km today, click link below to see your riding activity
