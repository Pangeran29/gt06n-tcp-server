# Database README

## Overview

This backend uses PostgreSQL as the first production storage layer for tracker data.

The database design is built around three tables:

- `devices`
- `device_locations`
- `device_heartbeats`
- `telegram_bot_state`
- `telegram_subscriptions`
- `telegram_payment_events`

The design is intentionally simple:

- one device master table
- one append-only table for location history
- one append-only table for heartbeat history

It also stores both:

- normalized fields for fast application queries
- raw protocol evidence for future reinterpretation

That matters because some protocol fields are still evolving, especially:

- `engine_status_guess`
- packet `0x26`
- extended Concox extra bytes

## Table Overview

### `devices`

Business meaning:

- this is the main identity table for a tracker
- one row represents one physical device, identified by IMEI

What it stores:

- IMEI
- SIM card number / label
- SIM card expiration date
- first seen / last seen
- last login / last heartbeat / last location time
- latest known location summary
- latest known heartbeat summary
- latest known peer address
- latest engine status guess

Why it exists:

- fast lookup of the latest state of a device
- avoids scanning history tables for the most recent status every time

### `device_locations`

Business meaning:

- this is the historical tracking table
- every decoded location packet becomes one row here

What it stores:

- device reference
- IMEI copy
- server receive time
- GPS timestamp from the packet
- protocol number / packet family
- latitude / longitude
- speed / course / course status
- satellite count / GPS info length
- extra packet bytes in hex
- peer address

Why it exists:

- keeps the full movement history of the device
- supports later features such as map history, route playback, and trip analysis

### `device_heartbeats`

Business meaning:

- this is the historical device-status table
- every decoded heartbeat packet becomes one row here

What it stores:

- device reference
- IMEI copy
- server receive time
- raw terminal info
- terminal info bits
- decoded heartbeat flags, including validated fields and neutral guess fields for unconfirmed bits
- engine status guess
- voltage level
- GSM signal strength
- alarm/language field
- peer address

Why it exists:

- keeps the full heartbeat/status history
- supports later investigation of ignition state, charging state, signal quality, and other operational conditions

## Business Relationship Between Tables

The relationship is:

- one row in `devices`
- many rows in `device_locations`
- many rows in `device_heartbeats`

In business terms:

- `devices` = current device identity and latest summary
- `device_locations` = tracking history for that device
- `device_heartbeats` = status history for that device

So:

- the `devices` table tells you the latest known state
- the history tables tell you how the device got there over time

## Why Raw And Parsed Data Are Stored Together

Some fields are already reliable enough for use, such as:

- IMEI
- latitude / longitude
- speed
- course
- satellite count

Some fields are still not fully final, such as:

- `engine_status_guess`
- future interpretation of `extra_data_hex`
- future handling of packet `0x26`

Because of that, the schema stores both parsed and raw values so the backend can improve later without losing historical meaning.

### `telegram_bot_state`

Business meaning:

- this is the small operational-state table for the Telegram bot
- it stores cursors and configuration that let the bot continue safely after restart

What it stores:

- last processed Telegram update id
- last notified heartbeat id
- admin chat id

Why it exists:

- prevents duplicate notification processing after restart
- keeps the bot decoupled from the TCP server
- lets the bot resume from PostgreSQL-backed state instead of in-memory state

### `telegram_subscriptions`

Business meaning:

- this is the current paid-access state for a Telegram user
- one row represents one subscription plan for one Telegram account
- the first plan is `monthly_stars`

What it stores:

- Telegram user id
- chat id snapshot for future payment/subscription messaging
- plan code
- subscription status
- current paid period start and end time
- created / updated timestamps

Allowed status values:

- `active`
- `expired`
- `canceled`
- `past_due`

Why it exists:

- gives the bot one fast place to check whether a Telegram user has paid access
- keeps entitlement state separate from raw payment events
- can support one-time 30-day Stars payments now and recurring Stars subscriptions later

The payment owner is the Telegram user, not the IMEI. This matches Telegram Stars payments, which are paid by a Telegram account. Device-level or shared-device access can be added later without changing the payment ledger.

### `telegram_payment_events`

Business meaning:

- this is the append-only payment ledger for Telegram Stars payment events
- one row represents one payment attempt/result known to the bot

What it stores:

- Telegram user id
- chat id snapshot
- optional subscription reference
- payment kind
- payment status
- plan code
- Stars currency (`XTR`)
- Stars amount
- access period length in days
- invoice payload
- Telegram payment charge id
- provider payment charge id, when present
- paid timestamp
- raw successful-payment JSON for future audit/debugging
- created / updated timestamps

Allowed payment status values:

- `pending`
- `paid`
- `failed`
- `refunded`

Why it exists:

- preserves payment history independently of the current subscription state
- provides idempotency via unique `invoice_payload` and `telegram_payment_charge_id`
- leaves room for refunds, failed attempts, and future recurring subscription renewals

For the initial one-time Stars payment flow, a successful payment should later create or extend `telegram_subscriptions.current_period_end_at` by 30 days. Feature gating and invoice handling are intentionally separate from this schema layer.
