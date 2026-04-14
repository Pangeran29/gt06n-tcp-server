# Telegram README

## Overview

This repo includes a separate Rust Telegram bot service.

Its purpose in the current phase is simple:

- read heartbeat rows from PostgreSQL
- send a Telegram notification for every new heartbeat row
- provide a small command surface for basic inspection

The bot does not read raw TCP packets directly. It depends on the data already stored by the TCP server.

## Current Behavior

The bot uses Telegram Bot API long polling.

It does two things in a loop:

- polls Telegram for bot commands
- polls PostgreSQL for new heartbeat rows that have not been notified yet

For v1, the notification rule is intentionally simple:

- every new heartbeat row produces one outgoing Telegram message

There is no deduplication or status-change-only filtering yet.

## Required Configuration

Set these values in `.env`:

```env
DATABASE_URL=postgres://postgres:postgres@localhost:5432/gt06n_tcp_server
TELEGRAM_BOT_TOKEN=
TELEGRAM_ADMIN_CHAT_ID=
TELEGRAM_POLL_TIMEOUT_SECS=30
TELEGRAM_HEARTBEAT_POLL_INTERVAL_MS=3000
```

Meaning:

- `DATABASE_URL`
  - required because the bot reads heartbeat and latest device data from PostgreSQL
- `TELEGRAM_BOT_TOKEN`
  - required to call Telegram Bot API
- `TELEGRAM_ADMIN_CHAT_ID`
  - optional fixed target chat id
- `TELEGRAM_POLL_TIMEOUT_SECS`
  - long polling timeout used for Telegram `getUpdates`
- `TELEGRAM_HEARTBEAT_POLL_INTERVAL_MS`
  - delay between each bot processing loop

## How To Run

Run the bot locally:

```powershell
cargo run --bin telegram_bot
```

Run the bot in release mode:

```powershell
cargo run --release --bin telegram_bot
```

## How To Bind A Chat

If `TELEGRAM_ADMIN_CHAT_ID` is not set, the first admin chat can be stored from Telegram itself.

Available commands:

- `/start`
- `/help`
- `/bind_me`
- `/last_heartbeat`
- `/latest_location`

The important one for first setup is:

- `/bind_me`

That command stores the current chat as the heartbeat notification destination.

## What The Bot Stores In PostgreSQL

The bot keeps its own operational state in the `telegram_bot_state` table.

That includes:

- `last_telegram_update_id`
- `last_notified_heartbeat_id`
- `admin_chat_id`

This allows the bot to restart without resending already-processed updates or losing its chat binding.

## Heartbeat Notification Content

Heartbeat notifications currently include:

- device IMEI
- server receive time
- raw terminal info
- terminal info bits
- engine status guess
- voltage level
- GSM signal strength

Important caution:

- `engine_status_guess` is still a heuristic interpretation and is not yet a fully validated ignition signal

## Current Limitations

- notifications are heartbeat-only for now
- every heartbeat row is sent; there is no deduplication yet
- the bot currently targets one admin chat
- `engine_status_guess` still needs real-world validation
- payment flows such as Telegram Stars are not implemented in this phase
