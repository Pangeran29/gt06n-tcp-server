# GT06N TCP Server

Rust backend for GT06 / Concox GPS trackers, Telegram bot operations, and Heartbeats subscription payments.

## Services

This repo ships four runtime components:

- `gt06n-tcp-server`: receives tracker packets and stores decoded data
- `telegram_bot`: handles Telegram registration, tracking features, and payment link creation
- `http_api`: serves operator/device APIs and Midtrans webhook
- `subscription_maintenance`: daily reminder and sanction job

## Main Flows

### Tracker ingestion

1. Device connects to the TCP server.
2. The server parses GT06 / Concox frames.
3. Latest device state is updated in `devices`.
4. History is appended to `device_locations` and `device_heartbeats`.

Supported packet families:

- login (`0x01`)
- heartbeat (`0x13`)
- classic location (`0x12`)
- extended location (`0x22`)

### Telegram onboarding

1. User sends `/start`.
2. If not bound yet, bot asks for IMEI.
3. Bot validates the IMEI and checks that the device exists.
4. Bot stores the binding in `telegram_users`.
5. Bound users land on the start menu.

### Subscription and payment

1. Payment starts from the Telegram bot.
2. Bot resolves the bound device from `telegram_users.bound_imei`.
3. Plan price is resolved from `devices.pricing_tier`.
4. Quote may include:
   - monthly plan price
   - late sanction
   - customer-referenced-device fee
   - one-time `devices.shipment_fee_idr`
5. Bot creates a pending `telegram_payment_events` row.
6. Bot creates a Midtrans Snap transaction and sends the link to Telegram.
7. Midtrans calls `POST /api/midtrans/webhook`.
8. Paid webhook updates the single current row in `telegram_subscriptions`.

Pricing behavior:

- `basic` = Rp 35.000 / 30 days
- `ojol` = Rp 30.000 / 30 days
- pricing tier is owned by `devices.pricing_tier`
- `devices.shipment_fee_idr` is charged only on the first successful paid payment for that device

### Reminder and sanction

`subscription_maintenance` runs daily and applies these rules:

- reminder at 5 days before subscription end
- overdue sanction at Rp 1.000 per day
- sanction caps at day 7
- subscription status moves to `past_due` after expiry

## HTTP API

Current routes:

- `GET /api/devices/{imei}/locations`
- `GET /api/devices/sim-cards`
- `PATCH /api/devices/{imei}/sim-card-expiration`
- `GET /api/subscriptions`
- `POST /api/midtrans/webhook`

## Telegram Bot Features

Current user-facing features:

- device binding via `/start`
- live tracking link
- latest health check
- ride session summary
- ride metrics
- payment link creation
- inactive-subscription prompts

Core commands:

- `/start`
- `/help`
- `/paysupport`
- `/terms`

## Configuration

Example `.env`:

```env
GT06_BIND_ADDR=0.0.0.0:5000
GT06_READ_BUFFER_CAPACITY=4096
RUST_LOG=info
DATABASE_URL=postgres://postgres:postgres@localhost:5432/gt06n_tcp_server
DATABASE_MAX_CONNECTIONS=10
DATABASE_WRITE_TIMEOUT_MS=5000
HTTP_API_BIND_ADDR=0.0.0.0:8080
TELEGRAM_BOT_TOKEN=
TELEGRAM_ADMIN_CHAT_ID=
TELEGRAM_POLL_TIMEOUT_SECS=30
TELEGRAM_HEARTBEAT_POLL_INTERVAL_MS=3000
MIDTRANS_SERVER_KEY=
MIDTRANS_CLIENT_KEY=
MIDTRANS_MERCHANT_ID=
MIDTRANS_IS_PRODUCTION=false
MIDTRANS_PAYMENT_EXPIRY_HOURS=24
MIDTRANS_BASIC_PLAN_PRICE_IDR=35000
MIDTRANS_OJOL_PLAN_PRICE_IDR=30000
```
202.155.157.221
Notes:

- database migrations run automatically when a database-backed binary starts
- `devices.pricing_tier` defaults to `basic`
- `ojol` assignment is operational and done by updating the device row
- existing pending payment links keep the amount stored when they were created

## Local Run

TCP server:

```bash
cargo run
```

Telegram bot:

```bash
cargo run --bin telegram_bot
```

HTTP API:

```bash
cargo run --bin http_api
```

Subscription maintenance:

```bash
cargo run --bin subscription_maintenance
```

Tests:

```bash
cargo test
```

## Docs

- [docs/bot.README.md](/Users/jojojow/Projects/heartbeats/gt06n-tcp-server/docs/bot.README.md)
- [docs/database.readme.md](/Users/jojojow/Projects/heartbeats/gt06n-tcp-server/docs/database.readme.md)
- [docs/deployment.README.md](/Users/jojojow/Projects/heartbeats/gt06n-tcp-server/docs/deployment.README.md)
- [docs/telegram-messages.README.md](/Users/jojojow/Projects/heartbeats/gt06n-tcp-server/docs/telegram-messages.README.md)
