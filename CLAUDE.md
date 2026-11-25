# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What This Is

fusion-link is the device-side connection daemon of the Fusion cloud portal: a resident macOS daemon that maintains an outbound-only WSS connection to the cloud broker, reports device health, relays cloud-initiated tasks to local `fusion-gateway` (:11432), and streams progress back. Zero inbound exposure is a hard product red line — never add a listener, never map local ports to the public internet.

Authoritative PRD: `architecture/fusion-link-prd-1125.md` (monorepo root). This is the M1 MVP: FR-1 (bind), FR-2 (heartbeat+health), FR-3 (task relay), FR-4 (event uplink), FR-6 (reconnect + outbox replay). Broker lives in the `fusion-ops` repo (`cmd/fusion-broker`), console in `fusion-site` — do not implement them here.

## Build / Test / Lint

```bash
cargo build --release
cargo test                                    # unit + integration (mock broker + mock gateway)
cargo clippy --all-targets -- -D warnings     # warnings are errors
cargo fmt --all --check
```

Run the daemon: `./start.sh start|stop|status|restart|log` (pidfile + log under `~/.fusion-link/`). CLI: `fusion-link bind <code> | unbind | status | run`.

## Architecture (src/)

- `protocol.rs` — WSS envelope `{v, type, ts, device_id, seq, payload}`; message types and error codes per PRD §7. serde renames map to dotted type strings (`task.dispatch`) — keep them exact.
- `session.rs` — the core loop: connect, hello, replay pending outbox, then `tokio::select!` over shutdown / heartbeat tick / uplink channel / broker messages. Disconnects return `SessionEnd::Disconnected` with a `[connected]` marker string so the outer loop only resets backoff after real contact. Uplink goes through the persistent outbox (at-least-once, broker dedups by seq).
- `outbox.rs` — seq assignment + JSON persistence (`~/.fusion-link/outbox.json`); push before send so a crash never loses an event.
- `health.rs` — concurrent loopback probes (2s timeout), snapshot has name/bool only (NFR-2).
- `gateway.rs` — config-driven task routing table; unsupported type = `E_UNSUPPORTED`, transport failure = `E_LOCAL_FAIL`.
- `keychain.rs` — credential via `security` CLI (service `fusion-link`), fingerprint from `IOPlatformUUID`.
- `backoff.rs` — exponential backoff with jitter, 1s base / 60s cap.
- `config.rs` — `~/.fusion-link/config.toml` is authoritative when present; env vars override (see README table).

## Conventions

- Match fusion-supervisor layout: lib + bin in one crate, edition 2024.
- Integration test `tests/session_e2e.rs` runs a real mock broker over TCP (`tokio_tungstenite::accept_async`) — extend it when adding protocol messages; keep it the source of truth for protocol behavior.
- All logs/comments English; 4-space indent; no docstrings; tracing for logging.
- Shutdown uses `CancellationToken` — watch channels are !Send inside `tokio::select!` state.
- Ports in defaults must match `architecture/port-registry.yaml` (config.rs `default_probes`).
