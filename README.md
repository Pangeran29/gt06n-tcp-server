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
  - location (`0x12`)
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
- satellite count

For heartbeat packets, it also decodes:

- terminal info
- voltage level
- GSM signal strength
- alarm/language field

For login packets, it decodes:

- device IMEI
- extra trailing login bytes, when present

## What Is Already Logged

When decoding succeeds, the current logger writes:

- device connection and disconnection events
- login acceptance with IMEI
- heartbeat details
- location details including:
  - latitude
  - longitude
  - speed
  - course
  - satellite count

One important detail:

- the GPS timestamp is already decoded internally
- but it is not yet included in the current location log output

So the timestamp exists in the backend model already, but it is not yet printed to the logs.

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

At the moment, the server is mainly the TCP ingestion layer for the tracker.

## Project Layout

- `src/main.rs`
  - application entrypoint and logging setup
- `src/config.rs`
  - configuration loading from environment variables and `.env`
- `src/server.rs`
  - TCP accept loop and per-device session handling
- `src/protocol.rs`
  - frame parsing, CRC validation, packet decoding, ACK generation, and hex formatting helpers
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
- location decoding
- ACK encoding
- checksum failure handling
- partial/truncated frame buffering
- hex formatting helper
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
- the current implementation is focused on `0x7878` frames
- some Concox firmware variants may send different payload layouts
- GPS timestamp is decoded but not yet included in log output
- there is no persistence layer yet
- there is no downstream API yet
- there is no dashboard yet

## TODO / Next Steps

Recommended next work items:

1. Add GPS timestamp to the location log output.
2. Capture and support the next real Concox packet variants seen from the device.
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
