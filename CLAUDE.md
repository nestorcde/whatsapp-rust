# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

@AGENTS.md

## Running a Single Test

```bash
# Unit/integration test by name (substring match)
cargo test -p whatsapp-rust -- test_name
cargo test -p wacore -- test_name

# Single E2E test file (requires mock server)
cargo test -p e2e-tests --test messaging -- test_send_text_message
```

## Full Workspace Layout

13 member crates with clear role separation:

| Crate | Role |
| ----- | ---- |
| `.` (whatsapp-rust) | Main client: Tokio runtime, high-level API, feature wiring |
| `wacore/` | Platform-agnostic protocol core (no Tokio, WASM-safe) |
| `wacore/appstate/` | App state sync protocol |
| `wacore/binary/` | Binary protocol marshaling (nibble/node encoding) |
| `wacore/derive/` | Proc-macro crate: `ProtocolNode`, `EmptyNode`, `WireEnum` |
| `wacore/libsignal/` | Signal protocol crypto |
| `wacore/noise/` | Noise protocol handshake |
| `waproto/` | Protobuf definitions (prost-generated, no logic) |
| `transports/tokio-transport/` | WebSocket via tokio-websockets + rustls |
| `storages/sqlite-storage/` | SQLite persistence (Diesel) |
| `http_clients/ureq-client/` | Blocking HTTP for CDN upload/download |
| `tests/e2e/` | E2E test suite against mock WhatsApp server |
| `tests/bench-integration/` | iai-callgrind benchmarks |

**Rule:** Protocol logic belongs in `wacore/`; runtime/storage wiring belongs in `whatsapp-rust` (root crate).

## Storage Backend Trait

`wacore::store::Backend` is the composite trait required by the client:

```text
Backend = SignalStore + AppSyncStore + ProtocolStore + MsgSecretStore + DeviceStore
```

`sqlite-storage` implements all five. Tests use an in-memory SQLite backend (UUID-keyed, no file I/O).

## Noise Handshake Pattern Selection

Three patterns (`src/handshake.rs`):

- **XX** (1.5 RTT) — first connect, pairing, or forced fallback
- **IK** (1 RTT) — reconnect with cached `serverStaticPub`
- **XXfallback** (1 RTT) — server rejects in-flight IK

Selection: use IK unless `ik_failures >= 1` or no cached server cert chain or cert is expired → use XX. A successful handshake resets `ik_failures = 0`; a crypto-fatal error during IK increments it and clears the cert.
