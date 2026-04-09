# GT06N TCP Server

This project is a Rust TCP server for `GT06N` GPS trackers. It listens for tracker connections, parses core GT06 binary packets, acknowledges the packets the device expects, and emits structured device events that can later be persisted or forwarded elsewhere.

## What It Does

- Accepts raw TCP connections from GT06-family trackers
- Detects and validates `0x7878` framed packets
- Verifies packet CRC before decoding
- Decodes these core packet types:
  - login (`0x01`)
  - heartbeat (`0x13`)
  - location (`0x12`)
- Sends ACK responses for login and heartbeat packets
- Logs structured events and exposes an internal event-handler boundary for future integrations

## Project Layout

- `src/main.rs`
  - starts the server, configures logging, and binds the TCP listener
- `src/config.rs`
  - reads runtime configuration from environment variables
- `src/server.rs`
  - owns the TCP accept loop and per-device session handling
- `src/protocol.rs`
  - GT06 framing, CRC validation, packet decoding, and ACK encoding
- `src/events.rs`
  - normalized event types plus the handler trait and logging implementation
- `tests/integration.rs`
  - end-to-end test that simulates a device TCP session

## Requirements

- Rust toolchain installed
- Network access the first time Cargo downloads dependencies

## Configuration

The server reads these environment variables. You can set them directly in PowerShell or place them in a local `.env` file at the project root.

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

The port is configured through `GT06_BIND_ADDR`.

Examples:

- `0.0.0.0:5000`
  - listen on port `5000` on all network interfaces
- `127.0.0.1:5000`
  - listen only on localhost, useful for local testing

If your GPS device needs to connect from outside your PC or LAN, do not use `127.0.0.1`. Use `0.0.0.0:<port>` and make sure your firewall/router allows that port.

### Option 1: PowerShell

Set values for the current terminal session:

```powershell
$env:GT06_BIND_ADDR="0.0.0.0:5000"
$env:RUST_LOG="info"
```

### Option 2: `.env` file

Create a file named `.env` in the project root. You can copy from `.env.example`.

Example `.env`:

```env
GT06_BIND_ADDR=0.0.0.0:5000
GT06_READ_BUFFER_CAPACITY=4096
RUST_LOG=info
```

The app now loads `.env` automatically on startup.

## How To Run

Run the server:

```powershell
cargo run
```

If you want it on a different port:

```powershell
$env:GT06_BIND_ADDR="0.0.0.0:6000"
cargo run
```

If you want to use `.env` instead, create the file first and then just run:

```powershell
cargo run
```

When the server starts successfully, it listens for incoming tracker TCP connections and logs decoded activity. You should see a startup log showing the bind address.

## How To Make Sure The Server Can Receive Connections

### Local machine check

Start the server:

```powershell
cargo run
```

Then, from another PowerShell window, test that the port is listening:

```powershell
Test-NetConnection -ComputerName 127.0.0.1 -Port 5000
```

If it works, `TcpTestSucceeded` should be `True`.

### Check from another device on your network

Replace the IP below with your Windows machine's LAN IP:

```powershell
Test-NetConnection -ComputerName 192.168.1.10 -Port 5000
```

If that fails:

- confirm `GT06_BIND_ADDR` is `0.0.0.0:5000` or your actual interface IP
- confirm Windows Firewall allows inbound TCP on that port
- confirm your router or network is not blocking the traffic

### Check with the real GT06N device

Configure the tracker to send to:

- your server public IP or LAN IP
- the same TCP port set in `GT06_BIND_ADDR`

Then watch the server logs. A successful device session usually looks like:

- TCP connection opened
- login packet accepted with IMEI
- heartbeat packet received
- location packet received

If the device connects repeatedly but never stays online, the tracker may be using a slightly different packet variant.

## How To Test

Run all tests:

```powershell
cargo test
```

The automated tests cover:

- config defaults and environment overrides
- login packet decoding
- heartbeat packet decoding
- location packet decoding
- ACK frame encoding
- checksum failure handling
- truncated frame buffering
- a simulated TCP client session that verifies login ACK, heartbeat ACK, and location event capture

## How To Check It With A Real Device

1. Run the server on a machine with a reachable public IP or a port-forwarded local IP.
2. Set the GT06N device to send TCP data to your server IP and port.
3. Start the server with `cargo run`.
4. Watch logs for:
   - device connection
   - login acceptance with IMEI
   - heartbeat reception
   - location reception with coordinates and speed
5. Confirm the device stays connected and continues sending heartbeat/location updates.

If the tracker connects but keeps retrying or disconnecting, that usually means the protocol ACK or packet variant differs from what the device expects.

## Current Limitations

- Only the core GT06N packet set is implemented right now
- Unsupported packet types are logged and ignored
- No persistence layer yet
- No HTTP/webhook forwarding yet
- No authentication, TLS, metrics, or admin API yet
- The implementation currently targets `0x7878` style frames, not extended `0x7979` variants

## What To Implement Next

Recommended next steps:

1. Add support for more GT06 alarm and status packet types.
2. Persist decoded events to a database such as PostgreSQL.
3. Add an HTTP or webhook forwarder for downstream applications.
4. Track device session state more deeply, including last-seen and connection health.
5. Add metrics and health endpoints for production monitoring.
6. Support protocol variants used by other GT06-family firmware revisions.
7. Add configuration files or containerization for easier deployment.

## Notes On Real-World GT06N Devices

GT06-family trackers often vary slightly by firmware. This server should work well as a clean starting point, but a real device may expose differences in:

- heartbeat payload details
- location packet extensions
- additional ACK expectations
- proprietary alarm/status messages

The easiest way to extend support is to capture real device packets and add them as tests before changing the decoder.
# gt06n-tcp-server
# gt06n-tcp-server
