# Telegram Message Catalog

This file is the current inventory of user-visible Telegram output in the backend.

Purpose:

- list every runtime Telegram text output in one place
- give each output a stable label for future localization
- show where the text currently lives in code

Scope:

- chat messages sent with `sendMessage`
- HTML payment-link messages sent with `sendMessage`
- callback toasts from `answerCallbackQuery`
- inline button labels
- stickers sent by the bot or webhook path

Note:

- the actual strings are still spread across `src/bot.rs`, `src/midtrans.rs`, `src/subscription_maintenance.rs`, and `src/api.rs`
- this document is the inventory first
- a later refactor can move them into one Rust module if you want

## Chat Messages

### `BOT_MSG_BIND_INVALID_IMEI`

Source: [src/bot.rs](/Users/jojojow/Projects/heartbeats/gt06n-tcp-server/src/bot.rs:847)

```text
IMEI must be exactly 15 numeric digits. Please send a valid IMEI.
```

### `BOT_MSG_BIND_ALREADY_BOUND`

Source: [src/bot.rs](/Users/jojojow/Projects/heartbeats/gt06n-tcp-server/src/bot.rs:854)

```text
Your Telegram account is already bound to a device.
```

### `BOT_MSG_BIND_DEVICE_NOT_FOUND`

Source: [src/bot.rs](/Users/jojojow/Projects/heartbeats/gt06n-tcp-server/src/bot.rs:865)

```text
That IMEI is not registered in the system yet. Please check the IMEI and try again.
```

### `BOT_MSG_BIND_DEVICE_ALREADY_TAKEN`

Source: [src/bot.rs](/Users/jojojow/Projects/heartbeats/gt06n-tcp-server/src/bot.rs:872)

```text
That device is already bound to another Telegram user.
```

### `BOT_MSG_BIND_SUCCESS`

Source: [src/bot.rs](/Users/jojojow/Projects/heartbeats/gt06n-tcp-server/src/bot.rs:881)

Template:

```text
Success. This Telegram account is now bound to IMEI {imei}.
```

### `BOT_MSG_NOT_BOUND_USE_START`

Source: [src/bot.rs](/Users/jojojow/Projects/heartbeats/gt06n-tcp-server/src/bot.rs:906), [src/bot.rs](/Users/jojojow/Projects/heartbeats/gt06n-tcp-server/src/bot.rs:1346), [src/bot.rs](/Users/jojojow/Projects/heartbeats/gt06n-tcp-server/src/bot.rs:1621)

```text
This Telegram account is not bound yet. Use /start first.
```

### `BOT_MSG_ANALYTICS_INVALID_DATE`

Source: [src/bot.rs](/Users/jojojow/Projects/heartbeats/gt06n-tcp-server/src/bot.rs:922)

```text
Invalid date. Please send it like:
2026-05-16
```

### `BOT_MSG_ANALYTICS_INVALID_RANGE`

Source: [src/bot.rs](/Users/jojojow/Projects/heartbeats/gt06n-tcp-server/src/bot.rs:923)

```text
Invalid date range. Please send it like:
2026-05-16 to 2026-05-16
```

### `BOT_MSG_START_BIND_PROMPT`

Source: [src/bot.rs](/Users/jojojow/Projects/heartbeats/gt06n-tcp-server/src/bot.rs:979)

```text
Welcome. Please send your device IMEI to bind this Telegram account.
```

### `BOT_MSG_HELP`

Source: [src/bot.rs](/Users/jojojow/Projects/heartbeats/gt06n-tcp-server/src/bot.rs:1963)

```text
Track your motor real time, get info when your motor on/off, get historical riding data

/start - Get the welcome message along with all feature of this bot
/help - Get this message
/paysupport - Get payment support contact
/terms - Read Heartbeats subscription terms
```

### `BOT_MSG_PAY_SUPPORT`

Source: [src/bot.rs](/Users/jojojow/Projects/heartbeats/gt06n-tcp-server/src/bot.rs:1964)

```text
For any questions, contact @jojojows
```

### `BOT_MSG_TERMS`

Source: [src/bot.rs](/Users/jojojow/Projects/heartbeats/gt06n-tcp-server/src/bot.rs:1965)

```text
Heartbeats is an online vehicle monitoring service. We provide affordable GPS tracking with advanced features through a monthly subscription. We manage the GPS platform, server infrastructure, internet data usage, and the Heartbeats application.

Monthly subscription includes:
- Real-time motorcycle tracking
- Instant engine ON/OFF notifications
- Ride analytics (distance, speed, riding time, and route map visualization)
- More features coming soon

Subscription Payment Policy:
Your subscription must be renewed within 7 days after your 30-day access period ends.
If payment is overdue, a penalty fee of Rp 1.000 per day will be applied until payment is completed.

GPS Device Policy:
The GPS device is provided as a loan unit.
If you stop using Heartbeats, you must return the device.
To arrange a return, please contact us via /paysupport.

Device Security Notice:
Heartbeats can track the GPS device location in real-time.
Do not attempt to steal, tamper with, or keep the device without permission.
```

### `BOT_MSG_UNKNOWN_COMMAND`

Source: [src/bot.rs](/Users/jojojow/Projects/heartbeats/gt06n-tcp-server/src/bot.rs:996)

Template:

```text
Unknown command: {command}. Use /help to see available commands.
```

### `BOT_MSG_START_STATUS`

Source: [src/bot.rs](/Users/jojojow/Projects/heartbeats/gt06n-tcp-server/src/bot.rs:2685)

```text
Welcome to @tryheartbeatsbot

Click /help for more information.
```

### `BOT_MSG_SUBSCRIPTION_MENU`

Source: [src/bot.rs](/Users/jojojow/Projects/heartbeats/gt06n-tcp-server/src/bot.rs:2688)

```text
Get full access to Heartbeats and monitor your motorcycle in real-time, anytime.
```

### `BOT_MSG_ENGINE_STATUS_ON`

Source: [src/bot.rs](/Users/jojojow/Projects/heartbeats/gt06n-tcp-server/src/bot.rs:2338)

Template:

```text
Motor Dinyalakan
Kalau ini bukan kamu, segera cek lokasi motor.
{timestamp_wib}
```

### `BOT_MSG_ENGINE_STATUS_OFF`

Source: [src/bot.rs](/Users/jojojow/Projects/heartbeats/gt06n-tcp-server/src/bot.rs:2342)

Template:

```text
Motor Dimatikan
Aktivitas terdeteksi pada motor kamu.
{timestamp_wib}
```

### `BOT_MSG_ENGINE_STATUS_FALLBACK_DIAGNOSTIC`

Source: [src/bot.rs](/Users/jojojow/Projects/heartbeats/gt06n-tcp-server/src/bot.rs:2315)

Template:

```text
Heartbeat update
IMEI: {imei}
Server time: {server_time_utc}
Engine status: {engine_status} (heuristic)
Terminal info: {terminal_info_raw} ({terminal_info_bits})
Voltage level: {voltage_level}
GSM signal: {gsm_signal_strength}
GPS tracking: {gps_tracking_on}
ACC high: {acc_high}
Vibration detected: {vibration_detected}
```

### `BOT_MSG_INACTIVE_SUB_ENGINE_ON`

Source: [src/bot.rs](/Users/jojojow/Projects/heartbeats/gt06n-tcp-server/src/bot.rs:2357)

```text
Motor Dinyalakan

Renew your subscription to receive live tracking, motor status, ride history, and theft alerts.
```

### `BOT_MSG_INACTIVE_SUB_ENGINE_OFF`

Source: [src/bot.rs](/Users/jojojow/Projects/heartbeats/gt06n-tcp-server/src/bot.rs:2360)

```text
Motor Dimatikan

Renew your subscription to receive live tracking, motor status, ride history, and theft alerts.
```

### `BOT_MSG_INACTIVE_SUB_FALLBACK`

Source: [src/bot.rs](/Users/jojojow/Projects/heartbeats/gt06n-tcp-server/src/bot.rs:2363)

```text
Motor activity detected.

Renew your subscription to receive live tracking, motor status, ride history, and theft alerts.
```

### `BOT_MSG_ENGINE_ON_CONFIRMATION`

Source: [src/bot.rs](/Users/jojojow/Projects/heartbeats/gt06n-tcp-server/src/bot.rs:2379)

```text
🚨 Engine ON Terdeteksi

Motor Anda baru saja dinyalakan.
Apakah ini Anda?
```

### `BOT_MSG_RIDE_SAFE`

Source: [src/bot.rs](/Users/jojojow/Projects/heartbeats/gt06n-tcp-server/src/bot.rs:2383)

```text
Ride safe - we'll keep tracking in the background for your safety.
```

### `BOT_MSG_SESSION_FINISHED`

Source: [src/bot.rs](/Users/jojojow/Projects/heartbeats/gt06n-tcp-server/src/bot.rs:2387)

```text
Ride session ended.
```

### `BOT_MSG_THEFT_WARNING`

Source: [src/bot.rs](/Users/jojojow/Projects/heartbeats/gt06n-tcp-server/src/bot.rs:2391)

```text
🚨 INDIKASI PENCURIAN

Motor ini dinyalakan bukan oleh Anda. ⚠️ Bertindak cepat — beberapa menit pertama sangat penting dalam kasus pencurian.

Tap tombol di bawah untuk mulai live tracking.
```

### `BOT_MSG_THEFT_LOCATION_MISSING`

Source: [src/bot.rs](/Users/jojojow/Projects/heartbeats/gt06n-tcp-server/src/bot.rs:2398)

```text
Latest location
Lokasi terakhir belum tersedia.
```

### `BOT_MSG_THEFT_ENGINE_OFF`

Source: [src/bot.rs](/Users/jojojow/Projects/heartbeats/gt06n-tcp-server/src/bot.rs:2413)

Template:

```text
🚨 THEFT ALERT

Your motorcycle engine was turned OFF during a suspected theft situation.

📍 Last Known Location:
{location_link_or_fallback}

GPS tracking is still active in battery mode while device power remains available.

Engine OFF detected at {timestamp_wib}.

⚠️ Act immediately: check the live location, share tracking access, or contact local authorities if needed.
```

### `BOT_MSG_STREAM_LOCATION`

Source: [src/bot.rs](/Users/jojojow/Projects/heartbeats/gt06n-tcp-server/src/bot.rs:2426)

Template:

```text
📍 Live Tracking Ready

Track your motorcycle in real-time:
{live_tracking_link_or_fallback}

You can share this link with someone you trust to help monitor or track your motorcycle.
```

Fallback link text inside the message:

```text
Live tracking link is not available yet.
```

### `BOT_MSG_CONTACT_SUPPORT`

Source: [src/bot.rs](/Users/jojojow/Projects/heartbeats/gt06n-tcp-server/src/bot.rs:2465)

```text
1. Hubungi Call Center 110
'Halo Polisi, saya ingin melaporkan pencurian motor yang baru saja terjadi. Posisi pelaku sedang terpantau di GPS saya. Mohon bantuan untuk pengejaran.'

2. Datangi SPKT Polsek/Polres
Langsung ke bagian SPKT (Sentra Pelayanan Kepolisian Terpadu). Tunjukkan aplikasi GPS yang sedang live kepada petugas. Polisi akan langsung berkoordinasi dengan tim Buser/Resmob untuk bergerak ke titik tersebut.

3. Bawa Bukti Kepemilikan
Siapkan STNK/BPKB (asli atau foto) dan KTP. Polisi butuh ini untuk memastikan itu benar motor Anda sebelum mereka melakukan penindakan atau penangkapan.

4. Minta Pendampingan Unit Lapangan
Setelah melapor, minta izin untuk mendampingi petugas (di mobil patroli) atau memberikan akses akun GPS Anda kepada petugas agar mereka bisa mengejar target secara akurat.

PENTING: Jangan mendatangi lokasi GPS sendirian. Biarkan polisi yang melakukan tindakan penggerebekan demi keselamatan Anda.
```

### `BOT_MSG_RIDE_SUMMARY`

Source: [src/bot.rs](/Users/jojojow/Projects/heartbeats/gt06n-tcp-server/src/bot.rs:2488)

Template:

```text
Ride Summary — {date}

🏍️ {total_distance_km:.2} km traveled
⏱️ {riding_time} riding time
⚡ Average speed: {average_speed_kph:.2} km/h

{start_time} → {end_time} WIB

🗺️ View Route
{history_link_or_fallback}

📍 Last Location
{latest_map_link_or_fallback}
```

Fallbacks inside the message:

```text
History link is not available yet.
Latest map link is not available yet.
```

### `BOT_MSG_LATEST_MOTOR_STATUS`

Source: [src/bot.rs](/Users/jojojow/Projects/heartbeats/gt06n-tcp-server/src/bot.rs:2577)

Template:

```text
📍 Motor Status

{map_link_or_fallback}

{movement_status} • Updated {relative_time}
Engine: {engine_status} • GPS: {signal_status} • Power: {battery_level}

{session_timing}{battery_warning_optional}
```

Fallbacks and nested labels used by this message:

```text
Location is not available yet.
unknown
MOVING at {speed} km/h
STATIONARY
UNKNOWN
Session active since {time_wib}
Last session ended at {time_wib}.
⚠️ GPS battery is empty. New updates may resume after the motorcycle is turned ON again.
{seconds}s ago
{minutes}m ago
{hours}h ago
Poor
Fair
OK
Excellent
Unknown
Empty
Very Low
Low
Medium
Full
```

### `BOT_MSG_LATEST_LOCATION`

Source: [src/bot.rs](/Users/jojojow/Projects/heartbeats/gt06n-tcp-server/src/bot.rs:2670)

Template:

```text
Latest location
IMEI: {imei}
GPS time: {gps_timestamp}
Server last seen: {last_seen_at}
Latitude: {latitude}
Longitude: {longitude}
Speed: {speed_kph} km/h
Course: {course} deg
Satellites: {satellite_count}
```

Fallback scalar text used here:

```text
unknown
```

### `BOT_MSG_ANALYTICS_PICK_RANGE`

Source: [src/bot.rs](/Users/jojojow/Projects/heartbeats/gt06n-tcp-server/src/bot.rs:1362)

Template:

```text
Choose range for {analytics_kind_label}.
```

Current kind labels:

- `driving session`
- `metrics`
- `total km`
- `total driving time`

### `BOT_MSG_ANALYTICS_CUSTOM_DATE_PROMPT`

Source: [src/bot.rs](/Users/jojojow/Projects/heartbeats/gt06n-tcp-server/src/bot.rs:1384)

```text
Send custom date in WIB:
YYYY-MM-DD

Example:
2026-05-16
```

### `BOT_MSG_ANALYTICS_CUSTOM_RANGE_PROMPT`

Source: [src/bot.rs](/Users/jojojow/Projects/heartbeats/gt06n-tcp-server/src/bot.rs:1386)

```text
Send custom date range in WIB:
YYYY-MM-DD to YYYY-MM-DD

Example:
2026-05-16 to 2026-05-16
```

### `BOT_MSG_ANALYTICS_SESSIONS_MONTH_UNSUPPORTED`

Source: [src/bot.rs](/Users/jojojow/Projects/heartbeats/gt06n-tcp-server/src/bot.rs:1411)

```text
History Perjalanan only supports one date at a time.
```

### `BOT_MSG_DRIVING_REPORT`

Source: [src/bot.rs](/Users/jojojow/Projects/heartbeats/gt06n-tcp-server/src/bot.rs:2873)

Template:

```text
🛣️ Driving Report — {date}

{session_count} sessions • {total_distance_km:.2} km traveled • {total_riding_time} riding time
Longest ride: {longest_ride}

{session_lines_or_empty_state}

📍 Full Day Route
{full_day_route_link_or_fallback}
```

Empty-state text and fallback used by this message:

```text
No driving sessions found on this date.
Full day route is not available yet.
ONGOING
```

### `BOT_MSG_TOTAL_KM_REPORT`

Source: [src/bot.rs](/Users/jojojow/Projects/heartbeats/gt06n-tcp-server/src/bot.rs:2952)

Template:

```text
Total KM
{analytics_range_label}

Total distance: {total_distance_km:.2} km
Average speed: {average_speed_kph:.2} km/h
```

### `BOT_MSG_RIDE_STATS_REPORT`

Source: [src/bot.rs](/Users/jojojow/Projects/heartbeats/gt06n-tcp-server/src/bot.rs:2964)

Template:

```text
🏍️ Ride Stats — {range_label}

{date_range} • {total_distance_km:.2} km traveled • {riding_time} riding time • {average_speed_kph:.1} km/h avg speed

⚠️ Regularly check your motorcycle condition for safety, including engine oil, tire pressure, and brake performance.
```

### `BOT_MSG_TOTAL_DRIVING_TIME_REPORT`

Source: [src/bot.rs](/Users/jojojow/Projects/heartbeats/gt06n-tcp-server/src/bot.rs:3003)

Template:

```text
Total Driving Time
{analytics_range_label}

Total driving time: {duration}
```

### `BOT_MSG_PAYMENT_LINK_HTML`

Source: [src/midtrans.rs](/Users/jojojow/Projects/heartbeats/gt06n-tcp-server/src/midtrans.rs:386)

Template:

```text
{plan_label}
{effective_base_amount} - 30 Days{shipment_line_optional}{fine_line_optional}{total_line_optional}

To activate your subscription, complete your payment using the link below:
<tg-spoiler>{payment_url}</tg-spoiler>

Payment link expires: {expires_at_wib}
```

Current plan labels:

- `Heartbeats Basic`
- `Heartbeats Ojol`

Optional nested lines:

```text
Shipment fee: {shipment_fee}
Late sanction: {fine_amount}
Total: {total_amount}
```

### `BOT_MSG_PAYMENT_SUCCESS`

Source: [src/bot.rs](/Users/jojojow/Projects/heartbeats/gt06n-tcp-server/src/bot.rs:3044), sent from [src/api.rs](/Users/jojojow/Projects/heartbeats/gt06n-tcp-server/src/api.rs:479)

Template:

```text
Payment Successful

Your Heartbeats access is now active until {active_until_wib}.

You're all set to start tracking and monitoring your motorcycle.
Type /start to begin or /help to see available features.
```

### `BOT_MSG_SUBSCRIPTION_PRE_EXPIRY_REMINDER`

Source: [src/subscription_maintenance.rs](/Users/jojojow/Projects/heartbeats/gt06n-tcp-server/src/subscription_maintenance.rs:279)

```text
Your Heartbeats subscription will end soon.

Please renew before it expires.
Late renewal is subject to a Rp 1.000/day sanction.
```

### `BOT_MSG_SUBSCRIPTION_OVERDUE_REMINDER`

Source: [src/subscription_maintenance.rs](/Users/jojojow/Projects/heartbeats/gt06n-tcp-server/src/subscription_maintenance.rs:284)

Template:

```text
Your Heartbeats subscription has expired.

Sanction: {fine_amount}
Please renew to continue full access.
```

## Callback Toasts

These are `answerCallbackQuery` texts, not normal chat messages.

### `BOT_TOAST_OPEN_BOT_CHAT`

Source: [src/bot.rs](/Users/jojojow/Projects/heartbeats/gt06n-tcp-server/src/bot.rs:1013)

```text
Please open the bot chat and try again.
```

### `BOT_TOAST_SUBSCRIPTION_REQUIRED`

Source: [src/bot.rs](/Users/jojojow/Projects/heartbeats/gt06n-tcp-server/src/bot.rs:1136)

```text
Subscription required.
```

### `BOT_TOAST_BIND_FIRST`

Source: [src/bot.rs](/Users/jojojow/Projects/heartbeats/gt06n-tcp-server/src/bot.rs:1173)

```text
Please bind your device with /start first.
```

### `BOT_TOAST_SESSION_NOT_FOUND`

Source: [src/bot.rs](/Users/jojojow/Projects/heartbeats/gt06n-tcp-server/src/bot.rs:1278)

```text
Session not found or already inactive.
```

### `BOT_TOAST_SESSION_MISMATCH`

Source: [src/bot.rs](/Users/jojojow/Projects/heartbeats/gt06n-tcp-server/src/bot.rs:1288)

```text
This session does not match the selected message.
```

### `BOT_TOAST_SESSION_ALREADY_ENDED`

Source: [src/bot.rs](/Users/jojojow/Projects/heartbeats/gt06n-tcp-server/src/bot.rs:1296)

```text
This session already ended.
```

### `BOT_TOAST_EMPTY_ACK`

Source: [src/bot.rs](/Users/jojojow/Projects/heartbeats/gt06n-tcp-server/src/bot.rs:1084), [src/bot.rs](/Users/jojojow/Projects/heartbeats/gt06n-tcp-server/src/bot.rs:1211), [src/bot.rs](/Users/jojojow/Projects/heartbeats/gt06n-tcp-server/src/bot.rs:1247), [src/bot.rs](/Users/jojojow/Projects/heartbeats/gt06n-tcp-server/src/bot.rs:1301)

Behavior:

- empty string is sent only to clear the Telegram callback loading state
- this has no visible text to the user

## Inline Button Labels

### `BOT_BTN_ENGINE_CONFIRM_YES`

Source: [src/bot.rs](/Users/jojojow/Projects/heartbeats/gt06n-tcp-server/src/bot.rs:2051)

```text
Yes, it's me
```

### `BOT_BTN_ENGINE_CONFIRM_NO`

Source: [src/bot.rs](/Users/jojojow/Projects/heartbeats/gt06n-tcp-server/src/bot.rs:2055)

```text
No, not me
```

### `BOT_BTN_THEFT_STREAM_LOCATION`

Source: [src/bot.rs](/Users/jojojow/Projects/heartbeats/gt06n-tcp-server/src/bot.rs:2067)

```text
stream location
```

### `BOT_BTN_THEFT_HEALTH_CHECK`

Source: [src/bot.rs](/Users/jojojow/Projects/heartbeats/gt06n-tcp-server/src/bot.rs:2071)

```text
health check
```

### `BOT_BTN_THEFT_CONTACT_SUPPORT`

Source: [src/bot.rs](/Users/jojojow/Projects/heartbeats/gt06n-tcp-server/src/bot.rs:2077)

```text
contact support
```

### `BOT_BTN_MENU_LIVE_TRACKING`

Source: [src/bot.rs](/Users/jojojow/Projects/heartbeats/gt06n-tcp-server/src/bot.rs:2096)

```text
Live Tracking
```

### `BOT_BTN_MENU_STATUS_TERKINI`

Source: [src/bot.rs](/Users/jojojow/Projects/heartbeats/gt06n-tcp-server/src/bot.rs:2100)

```text
Status terkini
```

### `BOT_BTN_MENU_HISTORY_PERJALANAN`

Source: [src/bot.rs](/Users/jojojow/Projects/heartbeats/gt06n-tcp-server/src/bot.rs:2107)

```text
History Perjalanan
```

### `BOT_BTN_MENU_AKTIVITAS_KENDARAAN`

Source: [src/bot.rs](/Users/jojojow/Projects/heartbeats/gt06n-tcp-server/src/bot.rs:2114)

```text
Aktivitas Kendaraan
```

### `BOT_BTN_RANGE_TODAY`

Source: [src/bot.rs](/Users/jojojow/Projects/heartbeats/gt06n-tcp-server/src/bot.rs:192)

```text
Today
```

### `BOT_BTN_RANGE_YESTERDAY`

Source: [src/bot.rs](/Users/jojojow/Projects/heartbeats/gt06n-tcp-server/src/bot.rs:193)

```text
Yesterday
```

### `BOT_BTN_RANGE_THIS_MONTH`

Source: [src/bot.rs](/Users/jojojow/Projects/heartbeats/gt06n-tcp-server/src/bot.rs:194)

```text
This month
```

### `BOT_BTN_RANGE_CUSTOM`

Source: [src/bot.rs](/Users/jojojow/Projects/heartbeats/gt06n-tcp-server/src/bot.rs:195)

```text
Custom
```

### `BOT_BTN_SUBSCRIBE`

Source: [src/bot.rs](/Users/jojojow/Projects/heartbeats/gt06n-tcp-server/src/bot.rs:2172)

```text
Subscribe
```

## Stickers

These are non-text Telegram outputs.

### `BOT_STICKER_ENGINE_ON`

Source: [src/bot.rs](/Users/jojojow/Projects/heartbeats/gt06n-tcp-server/src/bot.rs:1730)

Asset:

```text
asset/AnimatedSticker.tgs
```

### `BOT_STICKER_BIND_SUCCESS`

Source: [src/bot.rs](/Users/jojojow/Projects/heartbeats/gt06n-tcp-server/src/bot.rs:1751)

Asset:

```text
asset/AnimatedSticker - hi.tgs
```

### `BOT_STICKER_NOT_SUBSCRIBED`

Source: [src/bot.rs](/Users/jojojow/Projects/heartbeats/gt06n-tcp-server/src/bot.rs:1772)

Asset:

```text
asset/AnimatedSticker - no.tgs
```

### `BOT_STICKER_THEFT_WARNING`

Source: [src/bot.rs](/Users/jojojow/Projects/heartbeats/gt06n-tcp-server/src/bot.rs:1793)

Asset:

```text
asset/AnimatedSticker - not my motor.tgs
```

### `BOT_STICKER_PAYMENT_SUCCESS`

Source: [src/api.rs](/Users/jojojow/Projects/heartbeats/gt06n-tcp-server/src/api.rs:19)

Asset:

```text
asset/AnimatedSticker - payment success.tgs
```

## Defined But Not Currently Wired To Runtime

These templates exist in code but are not currently used by the runtime send path.

### `BOT_UNUSED_PAYMENT_LINK_LEGACY`

Source: [src/midtrans.rs](/Users/jojojow/Projects/heartbeats/gt06n-tcp-server/src/midtrans.rs:348)

This is the older fixed-price payment message template.

### `BOT_UNUSED_RIDE_SESSION_STATUS`

Source: [src/bot.rs](/Users/jojojow/Projects/heartbeats/gt06n-tcp-server/src/bot.rs:2612)

This is a current-session status template that is defined but not sent by the current bot flow.
