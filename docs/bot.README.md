# Telegram Bot

This document describes the behavior in [src/bot.rs](/Users/jojojow/Projects/heartbeats/gt06n-tcp-server/src/bot.rs).

## Responsibilities

- bind one Telegram account to one device IMEI
- show the main menu and protected tracking features
- poll heartbeats and send engine activity notifications
- manage ride sessions
- create Midtrans payment links
- block protected features when the subscription is inactive

## Runtime Loop

`TelegramBot::run` repeats:

1. poll Telegram updates
2. handle commands and callback queries
3. poll new `device_heartbeats`
4. send engine activity notifications
5. sleep for `telegram_heartbeat_poll_interval_ms`

The bot resumes safely using `telegram_bot_state`:

- `last_telegram_update_id`
- `last_notified_heartbeat_id`
- `telegram_admin_chat_id`

## Registration Flow

1. User sends `/start`.
2. If already bound, bot checks subscription state and shows the correct menu.
3. If not bound, bot creates or updates `telegram_users` with `registration_status = awaiting_imei`.
4. User sends IMEI.
5. Bot validates:
   - exactly 15 digits
   - device exists in `devices`
   - device is not bound to another Telegram user
6. Bot stores `bound_imei` and marks the user as `bound`.

Business rule:

- one Telegram user binds to one device
- one device is not allowed to be bound to two Telegram users

## Command Surface

- `/start`: onboarding or start menu
- `/help`: feature summary
- `/paysupport`: payment support contact
- `/terms`: subscription terms

## Start Menu and Access Gating

The bot checks `telegram_subscriptions` before protected actions.

- active subscription: full menu
- inactive or missing subscription: payment menu

Inactive users still receive limited engine activity messages, but protected tracking and analytics actions are blocked until renewal.

## Payment Flow

Payment always starts from the Telegram bot callback flow.

1. User taps the subscribe button.
2. Bot loads the Telegram user and bound device.
3. Bot resolves pricing tier from `devices.pricing_tier`.
4. Bot maps tier to plan code:
   - `basic` -> `monthly_basic`
   - `ojol` -> `monthly_ojol`
5. Bot builds a payment quote.

Quote components:

- monthly plan price
- sanction from current overdue subscription state
- customer-referenced-device fee when `devices.referenced_by_customer_id IS NOT NULL`
- one-time shipment fee from `devices.shipment_fee_idr` if the device has no prior `paid` payment event

6. Bot inserts a pending row into `telegram_payment_events`.
7. Bot creates a Midtrans Snap transaction.
8. Bot sends the payment link to Telegram.

Important invariants:

- payment price is device-driven, not Telegram-user-driven
- shipment fee is consumed by the first successful paid payment for that device
- old pending links keep the amount that was stored when they were created

## Webhook Result

The paid callback is handled by the HTTP API, but the bot-facing effect is:

- payment success message is sent to Telegram
- `telegram_subscriptions` keeps one current row per Telegram user
- the row is updated to the paid `plan_code`, period start, and period end

## Ride Session Flow

Ride sessions are stored in `telegram_engine_sessions`.

Active statuses:

- `pending_confirmation`
- `confirmed_safe`
- `reported_theft`

Terminal status:

- `finished`

### Engine on

When an `engine_status_guess = on` heartbeat arrives:

1. bot checks whether it belongs to the current active session
2. if not, old active sessions are finished first
3. bot opens a new session
4. bot sends confirmation buttons

### User response

- `Yes, it's me` -> `confirmed_safe`
- `No, not me` -> `reported_theft`

### Engine off

When an `engine_status_guess = off` heartbeat arrives:

1. bot loads active sessions for that `imei + chat_id`
2. bot closes confirmation keyboards if needed
3. bot finishes the session
4. bot sends ride summary
5. bot stores the summary message id for retry safety

## Tracking and Analytics

Current menu actions include:

- live tracking link
- latest device health check
- driving session list
- distance / riding time / average speed metrics

Tracking links are built against the Heartbeats client:

- live link uses `start_at`
- history link uses `start_at` and `end_at`

## Subscription Reminder Flow

Daily reminder and sanction logic is not executed by the bot loop itself. It is executed by `subscription_maintenance`, but the bot message surface depends on it.

User-visible behavior:

- one reminder per day from D-5 through D-1 before expiry
- overdue reminders once per day
- sanction increases Rp 1.000 per day
- protected features stay blocked while inactive
