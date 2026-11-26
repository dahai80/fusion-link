# fusion-link

Local-first connection daemon for the Fusion ecosystem. It is the device-side half of the fusion-mlx.cn cloud console: an always-on daemon on the user's Mac that maintains an **outbound-only** WSS connection to the cloud broker, reports device health, relays cloud-initiated tasks to the local `fusion-gateway`, and streams progress back to the browser.

One sentence: **fusion-link is the "online presence" and "reachability" of a local Fusion system on fusion-mlx.cn.**

- PRD: `architecture/fusion-link-prd-1125.md` (monorepo root)
- M1 MVP scope: FR-1 / FR-2 / FR-3 / FR-4 / FR-6
- Cloud broker: separate binary (`cmd/fusion-broker`) in the `fusion-ops` repo (PRD §11.1)

## Architecture

```
[browser] --HTTPS--> [fusion-site / VPS]
                          | broker (VPS, 127.0.0.1 only, nginx never exposes it)
                          ^ WSS (device-initiated, token + device fingerprint)
                      [fusion-link daemon (this repo, local Mac, fusion-sv managed)]
                          | loopback HTTP only
                      [fusion-gateway :11432 -> mlx/rag/artifacts/office (all local)]
```

Zero inbound exposure: the daemon only dials out. Tasks execute locally; only summaries, progress and optional artifact previews leave the machine (full-text files never do, by default).

## WSS protocol (PRD §7)

Envelope: `{v: 1, type, ts, device_id, seq, payload}`, JSON only, seq + ack for reliable ordered delivery.

| type | direction | meaning |
|---|---|---|
| `hello` / `hello_ack` | up | registration: credential, version, capabilities |
| `heartbeat` | up | 30s heartbeat + local service health snapshot |
| `task.dispatch` | down | task instruction (id, type, params, TTL) |
| `task.event` | up | state machine events: pending / running / awaiting_local_confirm / succeeded / failed / cancelled |
| `task.result` | up | result summary + artifact references |
| `config.update` | down | policy/quota changes |
| `ack` | both | delivery acknowledgement up to `ack_seq` |
| `error` | both | `E_AUTH` / `E_QUOTA` / `E_OFFLINE` / `E_UNSUPPORTED` / `E_LOCAL_FAIL` |

All uplink messages are journaled in a persistent outbox (`~/.fusion-link/outbox.json`) and replayed until the broker acks them — events survive disconnects and crashes (FR-6). Because the current broker does not ack heartbeats or task events, heartbeat entries are cleared on `hello_ack` and the journal is capped at 500 entries (oldest dropped first). Reconnect uses exponential backoff with jitter (1s base, 60s cap).

## Install

**Homebrew** (macOS / Linux):

```bash
brew install dahai80/tap/fusion-link
```

**pip** (ships the compiled binary as a Python wheel):

```bash
pip install fusion-link
```

**From source**: requires Rust 1.85+ (edition 2024).

```bash
cargo build --release
```

Prebuilt binaries (`.tar.gz` + sha256) for `aarch64/x86_64-apple-darwin` and `x86_64-unknown-linux-musl` are attached to each [GitHub release](https://github.com/dahai80/fusion-link/releases).

## Build & test

```bash
cargo build --release
cargo test                                    # unit + integration (mock broker & gateway)
cargo clippy --all-targets -- -D warnings
cargo fmt --all --check
```

## Usage

```bash
# 1. Bind device (one-time code from the fusion-mlx.cn console)
fusion-link bind <code>

# 2. Check state
fusion-link status

# 3. Run the daemon (foreground; start.sh manages pidfile + logs)
./start.sh start
./start.sh status
./start.sh log
./start.sh stop
```

Default task routes: `shell.chat` -> `POST /api/v1/shell/chat/completions`, plus office routes `office.spreadsheet` -> `/shell/office-api/v1/xlsx/parse`, `office.document` -> `/shell/office-api/v1/docx/write`, `office.slides` -> `/shell/office-api/v1/pptx/write` (all via `gateway_url`, 300s timeout). Unsupported task types are rejected with `E_UNSUPPORTED` and a reason (FR-3).

For gateway tasks the daemon exchanges the device credential for a short-lived (<= 15m) service ticket via `POST {gateway_url}/api/v1/link/ticket` (loopback, fusion-gateway #717 plane) and attaches it as `Authorization: Bearer <ticket>`. If the ticket plane is not enabled on the gateway, the daemon falls back to `gateway_token` (or no auth). A 401 mid-task surfaces as a visible task failure — never a silent retry.

Multipart tasks: when `params` contains `"_multipart": true`, the daemon sends `multipart/form-data` instead of JSON (required by office-api file endpoints). `file_b64` (base64) becomes the `file` part (named by `file_name`, typed by `file_mime`); all other params become form fields, JSON-encoded if not strings.

## Configuration

`~/.fusion-link/config.toml` (all keys optional; file is authoritative when present). Env overrides: `FUSION_LINK_BROKER_URL`, `FUSION_LINK_BIND_URL`, `FUSION_LINK_GATEWAY_URL`, `FUSION_LINK_GATEWAY_TOKEN`, `FUSION_LINK_STATE_DIR`, `FUSION_LINK_HEARTBEAT_SECS`.

| key | default |
|---|---|
| `broker_url` | `wss://fusion-mlx.cn/link/ws` |
| `bind_url` | `https://fusion-mlx.cn/api/link/bind` |
| `gateway_url` | `http://127.0.0.1:11432` |
| `heartbeat_secs` | `15` (broker read deadline is 30s; stay under it) |
| `state_dir` | `~/.fusion-link` |
| `task_routes` | `shell.chat` + 3 office routes |
| `probes` | mlx :11434, gateway :11432, rag :11436, artifacts :11451, office :11485, office-api :11492 |

Health snapshots carry service name + boolean status only (NFR-2); heartbeat stays under 1KB (NFR-3).

## Credential storage

Device credential (`device_id` + token + fingerprint) is stored in the macOS login keychain via the `security` CLI (service name `fusion-link`). The fingerprint derives from `IOPlatformUUID`. `fusion-link unbind` removes the credential and local outbox state.

## Supervision

`start.sh` follows the fusion-supervisor management contract (NFR-6): pidfile at `~/.fusion-link/fusion-link.pid`, logs at `~/.fusion-link/fusion-link.log`. Once registered, `fusion-sv` handles start/stop and crash recovery.

## Status

M1 MVP (2026-11): FR-1/2/3/4/6 implemented daemon-side. Broker (`fusion-ops` repo) and console pages are separate deliverables; the default `broker_url` is a placeholder until the broker endpoint goes live. M2 scope (FR-5/7/8/11/12) tracked in the PRD milestones.

## License

Apache-2.0
