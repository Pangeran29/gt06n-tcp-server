# GT06 / Concox TCP Server

This project is a Rust TCP server for `GT06` / `Concox`-family GPS trackers. It accepts raw tracker TCP connections, validates and parses the device protocol, sends the required ACK packets, and logs decoded device events.

Right now, this project is a device-facing TCP backend. It is not yet a full GPS tracking platform with database storage, API endpoints, or a map UI.

## Current Functionality

The server currently does these things:

- listens for raw TCP connections from GT06 / Concox devices
- accepts `0x7878` framed packets
- validates CRC before decoding packets
- decodes these packet types:
  - login (`0x01`)
  - heartbeat (`0x13`)
  - classic location (`0x12`)
  - Concox extended location (`0x22`)
- sends ACK responses for:
  - login
  - heartbeat
- accepts login packets where the first 8 bytes contain the IMEI and additional trailing login bytes may be present
- logs parsed device events through the built-in logging event handler
- logs packet payload hex when parsing fails, to make real-device protocol tuning easier

## What Data Is Already Decoded

For supported location packets, the backend already decodes:

- GPS timestamp
- latitude
- longitude
- speed
- course
- course status
- satellite count
- GPS info length
- extended trailing bytes for Concox `0x22` packets

For heartbeat packets, it also decodes:

- terminal info raw byte
- terminal info bit representation
- voltage level
- GSM signal strength
- alarm/language field
- a best-effort engine-state guess derived from heartbeat status bits

For login packets, it decodes:

- device IMEI
- extra trailing login bytes, when present

## What Is Already Logged

When decoding succeeds, the current logger writes:

- device connection and disconnection events
- login acceptance with IMEI
- heartbeat details including:
  - raw `terminal_info`
  - `terminal_info_bits`
  - `oil_and_electricity_connected`
  - `gps_tracking_on`
  - `alarm_active`
  - `charge_connected`
  - `acc_high`
  - `defense_active`
  - `engine_status_guess`
  - `voltage_level`
  - `gsm_signal_strength`
- location details including:
  - GPS timestamp
  - latitude
  - longitude
  - speed
  - course
  - course status
  - satellite count
  - extra data hex for extended location packets

## What This Project Does Not Have Yet

This server does not yet provide:

- a database
- a REST API
- a dashboard or frontend
- map visualization
- historical trip storage
- user authentication
- geofence/business logic
- alert notification delivery
- finalized alarm/status decoding for all Concox packet variants
- a fully validated ACC/engine-state mapping for every firmware variant

At the moment, the server is mainly the TCP ingestion layer for the tracker.

## Project Layout

- `src/main.rs`
  - application entrypoint and logging setup
- `src/config.rs`
  - configuration loading from environment variables and `.env`
- `src/server.rs`
  - TCP accept loop and per-device session handling
- `src/protocol.rs`
  - frame parsing, CRC validation, packet decoding, ACK generation, heartbeat flag decoding, and hex formatting helpers
- `src/events.rs`
  - normalized event models and the logging event handler
- `tests/integration.rs`
  - integration test that simulates a tracker session
- `docs/deployment.readme.md`
  - Ubuntu VPS deployment and `systemd` guide

## Requirements

- Rust toolchain installed
- network access the first time Cargo downloads dependencies

## Configuration

The server reads runtime settings from environment variables and also loads a local `.env` file automatically.

- `GT06_BIND_ADDR`
  - TCP bind address
  - default: `0.0.0.0:5000`
- `GT06_READ_BUFFER_CAPACITY`
  - initial per-connection read buffer size
  - default: `4096`
- `RUST_LOG`
  - log filter level
  - default: `info`

## Where To Set The Port

The listening port is configured through `GT06_BIND_ADDR`.

Examples:

- `0.0.0.0:5000`
  - listen on port `5000` on all interfaces
- `127.0.0.1:5000`
  - listen only on localhost

If your real GPS device must connect from outside your machine or VPS, use `0.0.0.0:<port>` and make sure the firewall allows that port.

### Option 1: PowerShell

```powershell
$env:GT06_BIND_ADDR="0.0.0.0:5000"
$env:RUST_LOG="info"
```

### Option 2: `.env`

Create a `.env` file in the project root:

```env
GT06_BIND_ADDR=0.0.0.0:5000
GT06_READ_BUFFER_CAPACITY=4096
RUST_LOG=info
```

## How To Run

Run the server:

```powershell
cargo run
```

For production-style local verification:

```powershell
cargo run --release
```

If the server starts correctly, you should see a startup log with the bind address.

## How To Confirm The Server Is Listening

### On Windows PowerShell

```powershell
Test-NetConnection -ComputerName 127.0.0.1 -Port 5000
```

If successful:

```text
TcpTestSucceeded : True
```

### From Git Bash On Windows

`Test-NetConnection` is a PowerShell command, so call it like this:

```bash
powershell.exe -Command "Test-NetConnection -ComputerName 127.0.0.1 -Port 5000"
```

### On Linux / VPS

```bash
ss -ltn | grep 5000
```

Expected:

```text
LISTEN ... 0.0.0.0:5000 ...
```

## How To Test

Run all tests:

```powershell
cargo test
```

The automated tests currently cover:

- config defaults and environment overrides
- login decoding
- login decoding with extra trailing bytes
- heartbeat decoding
- heartbeat terminal-info flag decoding
- classic location decoding
- Concox extended location decoding
- ACK encoding
- checksum failure handling
- partial/truncated frame buffering
- hex formatting helper
- terminal-info bit formatting helper
- simulated device TCP session

## How To Check With A Real Device

1. Run the server on a reachable machine or VPS.
2. Point the GT06 / Concox tracker to your server IP and port.
3. Watch the logs.
4. Look for:
   - device connection
   - login accepted with IMEI
   - heartbeat received
   - location received

If the device connects but repeatedly retries or disconnects, that usually means the device is using a slightly different protocol variant that still needs to be added.

## Current Limitations

- protocol support is still limited to a small core subset
- unsupported packet types are ignored and logged
- packet `0x26` is still not decoded
- the current implementation is focused on `0x7878` frames
- some Concox firmware variants may send different payload layouts
- engine-state classification is still a best-effort guess until it is validated against controlled on/off tests
- there is no persistence layer yet
- there is no downstream API yet
- there is no dashboard yet

## TODO / Next Steps

Recommended next work items:

1. Validate heartbeat `terminal_info` bits against real ACC on/off tests.
2. Decode packet `0x26` and determine whether it represents alarm, status, or alternate location data.
3. Add support for more alarm and status packet types.
4. Persist decoded events to a database such as PostgreSQL.
5. Add an HTTP API for latest location and device history.
6. Build a simple dashboard or map view for device positions.
7. Track device last-seen state and online/offline health.
8. Add metrics and health endpoints for production monitoring.
9. Support more GT06 / Concox firmware variations, including extended frame types if needed.
10. Harden production deployment by running under a dedicated Linux user instead of `/root`.

## Notes On Real Devices

GT06 / Concox devices often vary a little by model and firmware. The safest way to improve support is:

- collect real packet logs from the device
- keep raw payload hex when parsing fails
- add tests for those real packet examples
- then update the decoder based on the actual observed traffic

That approach is already reflected in the current backend design, which keeps the TCP session handling separate from packet decoding.

## Patch History

### 0.0

Implemented:

- created the initial Rust TCP server project scaffold
- added async TCP server runtime with `tokio`
- added config loading from environment variables
- added structured logging setup

Next needed:

- implement GT06 frame parsing
- implement login and heartbeat protocol support

### 0.1

Implemented:

- added GT06 `0x7878` frame detection
- added CRC validation
- added login (`0x01`) decoding
- added heartbeat (`0x13`) decoding
- added login and heartbeat ACK generation

Next needed:

- add location packet decoding
- add integration tests for end-to-end device flow

### 0.2

Implemented:

- added classic location (`0x12`) decoding
- added typed message and packet models
- added connection/session loop with per-device handling
- added unit tests and integration test coverage

Next needed:

- test against a real device
- handle real-world packet variations

### 0.3

Implemented:

- added `.env` loading support
- improved local run/test documentation
- added VPS deployment guide and `systemd` deployment notes

Next needed:

- validate against a real GT06 / Concox device on a public VPS
- adjust decoder based on actual field traffic

### 0.4

Implemented:

- validated real-device connectivity on a VPS
- confirmed login and heartbeat traffic from a real Concox device
- relaxed login parsing to accept trailing login bytes after the IMEI
- added logging for login extra-data and protocol parse failures

Next needed:

- identify and decode the real location packet variant used by the device

### 0.5

Implemented:

- captured and analyzed real `0x22` Concox packets from the device
- initially surfaced `0x22` traffic in debug logs with payload hex
- used real packet samples to guide decoder improvements

Next needed:

- decode `0x22` into normalized location data
- preserve unknown trailing bytes for future refinement

### 0.6

Implemented:

- added Concox extended location (`0x22`) decoding
- routed `0x22` through the normal location event flow
- preserved trailing `0x22` bytes as `extra_data`
- verified GPS coordinates from real device samples

Next needed:

- improve location logs with timestamp and extended metadata
- understand heartbeat status bits and engine state

### 0.7

Implemented:

- added richer heartbeat decoding helpers for terminal-info bit flags
- added best-effort engine-state classification from heartbeat status
- added GPS timestamp, course status, and extended extra-data hex to location logs
- improved current-state documentation

Next needed:

- validate engine on/off mapping with controlled real-world tests
- decode packet `0x26`
- prepare the event model for persistence

### Current State

Implemented:

- real Concox device can connect to the server over the internet
- login, heartbeat, and location data are being received and decoded
- current logs are rich enough to inspect movement, status, GPS quality, and protocol variations
- the server is stable enough to serve as the TCP ingestion layer for the next backend phase

Next needed:

- finalize engine-state interpretation
- decode remaining important packet variants
- start storing normalized device events in a database
