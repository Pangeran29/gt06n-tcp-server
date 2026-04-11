# Database Design

This document describes the first PostgreSQL storage layer for the GT06 / Concox TCP server.

## Goals

- preserve live tracker data without losing raw protocol evidence
- support fast inserts for location and heartbeat history
- support straightforward queries for latest device state
- avoid overcomplicating the first production schema

## Tables

### `devices`

Purpose:

- one stable row per IMEI
- holds current/latest-known summary state for fast reads

Examples of stored state:

- latest location
- latest heartbeat status
- latest peer address
- last seen timestamps
- latest engine status guess

### `device_locations`

Purpose:

- append-only history table for decoded location events
- keeps one row per received location packet

Stored fields include:

- device reference
- IMEI copy
- server receive time
- GPS timestamp
- protocol number / packet family
- latitude / longitude
- speed / course / course status
- satellite count / GPS info length
- extra packet data hex
- peer address

### `device_heartbeats`

Purpose:

- append-only history table for heartbeat/status packets
- keeps one row per received heartbeat event

Stored fields include:

- device reference
- IMEI copy
- server receive time
- raw terminal info
- terminal info bits
- decoded boolean flags
- engine status guess
- voltage level
- GSM signal strength
- alarm/language field
- peer address

## Why Store Both Parsed And Raw Fields

The current protocol parser is good enough for real production traffic, but some fields are still evolving.

Examples:

- `engine_status_guess` is still heuristic
- packet `0x26` is not decoded yet
- extra bytes in extended Concox packets may gain meaning later

Because of that, the schema stores:

- normalized fields for product queries
- raw evidence for future reinterpretation

This avoids losing useful data when the parser improves later.

## Insert / Update Flow

### Login

- upsert the `devices` row by IMEI
- update first seen / last seen / last login / latest peer address

### Heartbeat

- ensure the device row exists
- insert a row into `device_heartbeats`
- update latest heartbeat summary fields in `devices`

### Location

- ensure the device row exists
- insert a row into `device_locations`
- update latest location summary fields in `devices`

## Current Limitations

- `engine_status_guess` should not yet be treated as authoritative ignition truth
- packet `0x26` is still pending parser support
- no partitioning or retention jobs are implemented yet
- no PostGIS or advanced timeseries extensions are used in v1

## Example Query Use Cases

Latest known device position:

```sql
SELECT imei, latest_latitude, latest_longitude, latest_gps_timestamp, last_seen_at
FROM devices
WHERE imei = '866221070478388';
```

Recent location history for one device:

```sql
SELECT gps_timestamp, latitude, longitude, speed_kph, course
FROM device_locations
WHERE imei = '866221070478388'
ORDER BY server_received_at DESC
LIMIT 100;
```

Recent heartbeat history for one device:

```sql
SELECT server_received_at, terminal_info_raw, terminal_info_bits, engine_status_guess
FROM device_heartbeats
WHERE imei = '866221070478388'
ORDER BY server_received_at DESC
LIMIT 100;
```
