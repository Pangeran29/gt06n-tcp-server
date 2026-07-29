# Database

This backend uses PostgreSQL for device state, history, Telegram state, and payment state.

## Table Groups

Operationally, the schema has four groups:

- current device state
- append-only device history
- Telegram bot state
- subscription and payment state

## Device State

### `devices`

One row per physical GPS device.

Main fields:

- `imei`
- `pricing_tier`
- `shipment_fee_idr`
- latest location summary
- latest heartbeat summary
- latest engine status guess
- SIM card metadata

Business meaning:

- this is the source of truth for the current device snapshot
- payment pricing is resolved from this table
- shipment fee is owned by the device, not the Telegram user

### `device_locations`

Append-only GPS location history.

Main fields:

- `device_id`
- `imei`
- `server_received_at`
- `gps_timestamp`
- `latitude`
- `longitude`
- `speed_kph`
- `course`
- `protocol_number`

### `device_heartbeats`

Append-only heartbeat and status history.

Main fields:

- `device_id`
- `imei`
- `server_received_at`
- heartbeat bit fields
- voltage / GSM signal
- `engine_status_guess`
- `protocol_number`

### `device_distance_odometer`

Current tracked-distance total and processing checkpoint per device.

Business meaning:

- updated transactionally with each persisted GPS location
- counts only plausible movement segments
- represents distance observed by Heartbeats, not the motorcycle's physical odometer

### `device_distance_daily`

Accepted tracked distance grouped by WIB calendar date.

Business meaning:

- provides stable dates for each 1.000 km service milestone
- supports fast service-history responses without scanning raw GPS history

## Telegram State

### `telegram_users`

One row per Telegram user known by the bot.

Main fields:

- `telegram_user_id`
- `chat_id`
- `bound_imei`
- `registration_status`

Business meaning:

- stores onboarding and device binding state
- payment still belongs to the Telegram user, but price comes from the bound device

### `telegram_bot_state`

Small key-value operational state for bot restart safety.

Used for:

- last processed Telegram update
- last notified heartbeat
- bound admin chat id

### `telegram_engine_sessions`

Ride session state per `imei + chat_id`.

Business meaning:

- stores active or finished ride sessions
- supports confirmation flow, theft flow, and ride summaries

## Subscription and Payment

### `telegram_subscriptions`

Current-state table for access control.

Business rule:

- one current row per Telegram user

Main fields:

- `telegram_user_id`
- `chat_id`
- `plan_code`
- `status`
- `current_period_start_at`
- `current_period_end_at`

Current plan codes:

- `monthly_basic`
- `monthly_ojol`

Business meaning:

- this is not payment history
- this is the current paid-access state only
- when plan changes, the next successful payment updates this same row

### `telegram_payment_events`

Append-only payment ledger.

Business rule:

- many rows per Telegram user
- many rows per device over time

Main fields:

- `telegram_user_id`
- `chat_id`
- `device_id`
- `imei`
- `plan_code`
- `payment_status`
- `gross_amount_idr`
- `provider_order_id`
- `provider_transaction_id`
- `expires_at`
- `paid_at`
- raw Midtrans payload fields

Business meaning:

- stores every payment attempt and result
- exact billed plan code is stored at pending-payment creation time
- first successful paid payment for a device is what consumes `devices.shipment_fee_idr`

## Payment Logic Across Tables

Quote creation reads:

- `telegram_users.bound_imei`
- `devices.pricing_tier`
- `devices.shipment_fee_idr`
- `telegram_subscriptions.current_period_end_at`
- `telegram_payment_events` paid history by `device_id`

Rules:

- plan price comes from `devices.pricing_tier`
- sanction comes from the user’s current overdue subscription state
- shipment fee is added only if the device has no prior `paid` payment row
- webhook updates `telegram_subscriptions`, but does not recompute the quote

## Current Ownership Model

- device owns pricing tier
- device owns shipment fee
- Telegram user owns the active subscription row
- payment ledger records both Telegram user and device linkage
