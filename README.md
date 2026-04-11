# GT06 / Concox TCP Server

## Overview Project

This project is a Rust TCP backend for `GT06` / `Concox` GPS trackers.

Its job is to:

- accept raw TCP connections from the GPS device
- validate and decode tracker packets
- send the ACK packets the device expects
- log decoded device activity
- persist decoded data into PostgreSQL

This backend is already suitable as the ingestion layer for a tracking system. It is not yet a complete product with public APIs, dashboards, authentication, or business workflows.

The server currently supports these protocol packets:

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
```

Notes:

- if `DATABASE_URL` is not set, the server still runs, but without PostgreSQL persistence
- if `DATABASE_URL` is set, the server will connect to PostgreSQL and run migrations automatically at startup
- the PostgreSQL database itself must already exist before the server starts

### 2. Run locally

```powershell
cargo run
```

For release mode:

```powershell
cargo run --release
```

### 3. Check that it is listening

Windows PowerShell:

```powershell
Test-NetConnection -ComputerName 127.0.0.1 -Port 5000
```

Linux / VPS:

```bash
ss -ltn | grep 5000
```

### 4. Run tests

```powershell
cargo test
```

## What This Has

### TCP / Protocol

- async TCP server using `tokio`
- GT06 `0x7878` frame parsing
- CRC validation
- login ACK support
- heartbeat ACK support
- support for real Concox login packet variants with extra trailing bytes

### Decoded Data

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

More detail:

- database design: [database.README.md](/d:/jonathan/gps-tracker/tcp-server/docs/database.README.md)
- deployment process: [deployment.README.md](/d:/jonathan/gps-tracker/tcp-server/docs/deployment.README.md)

## What Need To Be Implemented

The main remaining work is:

- validate `engine_status_guess` against real ignition on/off tests
- decode packet `0x26`
- support more Concox alarm and status packet variants
- build a read API for latest device state and history
- add dashboard / map UI
- add retention and archival strategy for long-running GPS history
- improve production hardening, especially running the service under a dedicated Linux user instead of `/root`
