# Telegram GPS Tracking Bot

This document describes the Telegram bot implemented in `src/bot.rs`. The bot is the Telegram-facing client for motorcycle GPS tracking. It binds a Telegram user to a GPS device IMEI, watches incoming device heartbeats, notifies the user when the motorcycle engine turns on or off, manages ride sessions, and provides inline actions for live tracking, diagnostics, and emergency support.

## Main Responsibilities

- Register and bind a Telegram account to one GPS device IMEI.
- Poll Telegram updates using `getUpdates`.
- Poll stored device heartbeats from PostgreSQL.
- Detect notifiable engine status changes from `engine_status_guess`.
- Open, update, and finish ride sessions.
- Send Telegram messages, inline keyboards, and stickers.
- Build live-tracking and history-tracking links for the web client.
- Generate ride summaries from stored GPS location history.

## Runtime Loop

`TelegramBot::run` runs forever:

1. Poll Telegram updates.
2. Handle text messages and callback queries.
3. Poll new device heartbeats.
4. Send heartbeat-driven notifications.
5. Sleep for `telegram_heartbeat_poll_interval_ms`.

The bot stores two polling cursors in `telegram_bot_state`:

- `last_telegram_update_id`
- `last_notified_heartbeat_id`

These cursors let the bot resume after restart without reprocessing old Telegram updates or old heartbeats.

## User Registration

The registration flow starts with `/start`.

If the Telegram user is already bound to an IMEI, the bot sends the start menu. If not, the bot stores the user in `telegram_users` with `registration_status = awaiting_imei` and asks the user to send their device IMEI.

When a normal text message arrives while the user is awaiting IMEI:

1. The bot validates that the IMEI is exactly 15 numeric digits.
2. It checks that the IMEI exists in `devices`.
3. It checks that the IMEI is not already bound to another Telegram user.
4. It updates `telegram_users` with `bound_imei` and `registration_status = bound`.
5. It sends a success message, sticker, and the start menu.

Current registration policy is one Telegram user per IMEI. The notification/session code is shaped around `imei + chat_id`, but `is_device_bound_to_another_user` prevents another Telegram user from binding the same device.

## Commands

### `/start`

Starts registration for new users or shows the start menu for already-bound users.

### `/help`

Sends a short feature description and command list.

### Unknown slash commands

The bot replies with an unknown-command message and points the user to `/help`.

## Inline Buttons

### Engine confirmation buttons

Shown when the bot detects a new engine-on session.

- `Yes, it's me`
  - Callback: `engine_session:yes`
  - Marks the active session as `confirmed_safe`.
  - Sends a ride-safe message.
  - Sends the engine-on sticker.

- `No, not me`
  - Callback: `engine_session:no`
  - Marks the active session as `reported_theft`.
  - Sends a theft warning message.
  - Shows theft-action buttons.

These status changes do not set `resolved_at`. The session is still active until it is finished by engine-off or by the next qualifying engine-on event.

### Start menu buttons

Shown after binding and when an already-bound user sends `/start`.

- `stream location`
  - Callback: `theft_alert:stream_location`
  - Sends a live-tracking link.

- `health check`
  - Callback: `theft_alert:check_latest_status`
  - Sends the latest motor diagnostics report.

### Theft-action buttons

Shown after the user reports that an engine-on event was not them.

- `stream location`
  - Callback: `theft_alert:stream_location:{session_id}`
  - Sends a live-tracking link based on that session start time.

- `health check`
  - Callback: `theft_alert:check_latest_status:{session_id}`
  - Sends diagnostics using that session context.

- `contact support`
  - Callback: `theft_alert:contact_support:{session_id}`
  - Sends emergency support/reporting guidance.

## Heartbeat Notification Flow

The bot reads new rows from `device_heartbeats` where `id > last_notified_heartbeat_id`.

Only these heartbeat statuses are notifiable:

- `engine_status_guess = on`
- `engine_status_guess = off`

Other engine status guesses are skipped.

For each notifiable heartbeat, the bot fetches all Telegram chats bound to the heartbeat IMEI and processes the notification for each chat.

## Ride Session Model

Ride sessions are stored in `telegram_engine_sessions`.

Important fields:

- `imei`: GPS device identity.
- `chat_id`: Telegram chat that owns this session.
- `trigger_heartbeat_id`: heartbeat that opened the session.
- `prompt_message_id`: Telegram confirmation message id.
- `ride_status_message_id`: Telegram ride summary message id, used to make retries safer.
- `session_status`: current session state.
- `created_at`: ride/session start time.
- `resolved_at`: session finish time.

The bot now treats these statuses as active ride sessions:

- `pending_confirmation`
- `confirmed_safe`
- `reported_theft`

The terminal status is:

- `finished`

`resolved_at` is set only when the session becomes `finished`.

## Standard Session Lifecycle

### Engine turns on

When an engine-on heartbeat arrives, the bot checks the last known engine-on heartbeat time for that `imei + chat_id`.

If the previous engine-on heartbeat is less than `ENGINE_ON_ALERT_COOLDOWN_SECS` old, the bot treats the heartbeat as part of the existing session and only updates notification state.

If the gap is equal to or greater than the threshold:

1. Fetch all active sessions for the same `imei + chat_id`.
2. Finish those active sessions first.
3. Send ride summaries for the finished sessions.
4. Send a new engine-on confirmation prompt.
5. Create a new `pending_confirmation` session.
6. Store the heartbeat notification state as `on`.

This handles low-battery or missing-off cases. If the device never sends an engine-off heartbeat, the next qualifying engine-on heartbeat closes the previous active ride before opening a new one.

### User confirms the ride

If the user taps `Yes, it's me`, the session moves:

```text
pending_confirmation -> confirmed_safe
```

The ride stays active.

### User reports possible theft

If the user taps `No, not me`, the session moves:

```text
pending_confirmation -> reported_theft
```

The ride stays active, but the user receives theft-related action buttons.

### Engine turns off

When an engine-off heartbeat arrives:

1. Fetch all active sessions for the same `imei + chat_id`.
2. Clear pending confirmation keyboards where needed.
3. Send `Ride session ended.`
4. Compute and send the ride summary.
5. Store the ride summary Telegram message id in `ride_status_message_id`.
6. Mark the session as `finished`.
7. Set `resolved_at`.
8. Store the heartbeat notification state as `off`.

If there are no active sessions, the bot sends a normal engine-off notification instead.

## Retry Safety

Ride finishing is partially retry-safe.

The bot sends the ride summary, stores the summary message id in `ride_status_message_id`, and then marks the session `finished`.

If the bot retries after the summary id was stored but before the session was resolved, it sees `ride_status_message_id` and resolves the session without sending another summary.

There is still one small unavoidable gap: if Telegram accepts the summary message but the bot fails before storing `ride_status_message_id`, a retry can send the summary again.

## Live Tracking

Live tracking links use:

```text
https://hearthbeats-client.vercel.app/live-tracking/{imei}?start_at={timestamp}
```

The start time is selected in this order:

1. Explicit callback session `created_at`.
2. Latest session `created_at`.
3. Latest device `last_seen_at`.

If none are available, the bot tells the user that the live tracking link is not available yet.

## Ride History

Ride summary links use:

```text
https://hearthbeats-client.vercel.app/live-tracking/{imei}?start_at={timestamp}&end_at={timestamp}
```

`start_at` comes from the session `created_at`. This is the trigger heartbeat timestamp, not bot processing time.

`end_at` comes from the heartbeat that finished the session, either engine-off or the next qualifying engine-on heartbeat.

## Ride Summary

Ride summaries include:

- Ride date.
- Start and end time in WIB.
- Duration.
- Total distance.
- Average speed.
- History route link.
- Latest Google Maps location link.

Distance is calculated from `device_locations` using haversine distance between consecutive stored latitude/longitude points in the ride window.

## Health Check

The health-check button sends a motor diagnostics report.

It includes:

- Last known location link.
- Movement status based on latest speed.
- Last update age.
- Engine status.
- GSM signal quality.
- GPS tracking mode.
- GPS battery level.
- Session state.
- Session start and end time.
- Battery-empty warning when `voltage_level = 0`.

If no real session exists, the bot builds an in-memory fallback session with status `bound`.

## Contact Support

The contact-support action sends emergency guidance in Indonesian. It tells the user to contact police, go to SPKT, prepare ownership documents, request field assistance, and avoid approaching the GPS location alone.

## Data Tables Used

### `telegram_users`

Stores Telegram identity, chat id, bound IMEI, and registration status.

### `telegram_bot_state`

Stores polling cursors and other bot state values.

### `telegram_device_notifications`

Stores the latest notification status per `imei + chat_id`:

- `last_status`
- `last_message_id`
- `last_heartbeat_id`

### `telegram_engine_sessions`

Stores ride/session lifecycle state.

### `device_heartbeats`

Source of engine status, GPS tracking status, voltage level, GSM signal, and other heartbeat metadata.

### `devices`

Source of latest known device location fields.

### `device_locations`

Source of historical location points used for ride distance and history summaries.

## Maintenance Notes

- Active session behavior should stay status-agnostic. Engine-off and next qualifying engine-on should finish any active session, regardless of whether it is pending, safe-confirmed, or theft-reported.
- `confirmed_safe` and `reported_theft` should not set `resolved_at`; they are still active states.
- `created_at` should remain based on the trigger heartbeat timestamp so ride summaries reflect actual device activity.
- `ride_status_message_id` is used as a retry marker for ride summaries.
- The heartbeat cursor is currently global. If one chat fails after another chat succeeds, a retry may duplicate successful messages for the earlier chat.
- Current registration prevents multiple Telegram users from binding the same IMEI, even though notification/session state is modeled by `imei + chat_id`.
