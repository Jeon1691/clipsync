# Public interface reference

This reference describes the interfaces implemented in this repository: the
`clipsync` and `clipsync-relay` commands, relay HTTP/WebSocket messages, optional
relay webhooks, and the mobile UniFFI API. See [the protocol](protocol.md) for
limits and cryptographic framing, [the security model](security.md) for trust
boundaries, and [mobile development](mobile.md) for building the native apps.

The linked source definitions are authoritative when changing an interface.
IDs are JSON strings; generated IDs use ULIDs. Every field typed as a room,
device, message, transfer or pairing-session ID must be 1–64 bytes of ASCII
letters, digits, `-`, `_` and `.`, with `.` only between other characters (no
leading or trailing `.`, no `..`); anything else fails to decode
([`validate_id`](../crates/clipsync-protocol/src/ids.rs)). Times are RFC 3339 UTC strings
unless stated otherwise. Fields described as nullable accept `null`; optional
request fields may also be omitted. Other listed request fields are required.
Tokens, pairing codes, clipboard data, and full room/configuration output can
contain secrets: redact them before sharing diagnostics.

## Client command line

Definitions: [Clap commands](../crates/clipsync-cli/src/cli.rs),
[dispatch and output](../crates/clipsync-cli/src/main.rs),
[core operations](../crates/clipsync-core/src/lib.rs), and
[exit codes](../crates/clipsync-core/src/error.rs).
Run `clipsync --help` or `clipsync <command> --help` for syntax. A subcommand is
required. All commands accept `-h`/`--help` and `-V`/`--version`.

### Global flags and output conventions

| Flag | Behavior |
| --- | --- |
| `--json` | Print command result objects/arrays as compact JSON, one value per line. Streaming pairing can emit multiple values. |
| `--yes` | Confirm room removal without an interactive confirmation. |
| `--non-interactive` | Never prompt; the current `room leave` handler also treats this as confirmation. A verification-code prompt fails instead (pass the code to `room confirm`, or `--insecure-skip-sas`). |

Successful commands exit with `0`. Ordinary runtime errors print
`error: <message>` to **stderr**, including with `--json`; they do not have a
general JSON error envelope. Clap reports invalid arguments on stderr with exit
code `2`. Without `--json`, result output is human-readable and may be empty for
successful state-changing commands. Diagnostic tracing can appear on stdout;
`RUST_LOG` controls its level and `--json` also selects JSON tracing.

### Commands and result values

In the table, JSON describes stdout with `--json`. Unspecified normal-mode
output is empty; errors follow the convention above.

| Command and inputs | Effect and normal-mode output | JSON result |
| --- | --- | --- |
| `init [--relay-url URL]` | Create/reuse the local identity; optionally save a relay URL. Print device ID and fingerprint. | `{device_id: string, fingerprint: string}` |
| `room create [--ttl DURATION] [--no-auto-sync] [--foreground] [--insecure-skip-sas]` | Print a six-digit pairing code, room, and expiry. Default: detach the pairing wait and return after the offer is available; answer the verification code later with `room confirm` or `room reject`. `--foreground` (alias `--wait`) waits in this terminal, asks for the verification code like `room join`, and prints `paired` on success. | First `{event: "pairing_code", pairing_code: string, room_id: string, expires_at: time}`. Background mode adds `{background: true, pid: integer}`; foreground mode later emits the SAS event below, then `{ok: true, pairing_code: string, room_id: string}`. |
| `room join CODE [--no-auto-sync] [--insecure-skip-sas]` | Join using the code string, preserving any leading zero. After key confirmation, print the verification code and the other device's name and read one answer line from stdin: the code shown on the other device confirms, and so does `y`/`yes` when stdin is a terminal; anything else or end of input rejects and stores nothing. Then wait for pairing and print the room ID. | `{event: "sas_confirmation", sas: string, peer_device_name: string}`, then `{room_id: string, role: "member"}` |
| `room confirm [CODE]` | Answer a background `room create`. `CODE` must be the six digits shown on the other device (spaces or dashes between groups are allowed); anything else, including `yes`, fails at once and records nothing. Waits for the joiner if needed, then compares `CODE` with this device's code, or without `CODE` shows the code and prompts like `room join`; that prompt ends with an error, without waiting for input, if the pairing fails, expires, is rejected or is removed meanwhile. A mismatch aborts the pairing. On a match, waits for the worker and prints `paired`. | Without `CODE`, the SAS event above; then `{ok: true, room_id: string}`. |
| `room reject` | Abort a background `room create`. At the verification step, waits until the worker has stopped, then reports that nothing was stored; fails if the code was already confirmed. Before a device joins, records the rejection (the worker stops when one joins) and lets a new `room create` start at once. | `{ok: true, rejected: true}`; before a join also `joined: false` |
| `room leave [--yes]` | Stop/uninstall the login daemon, remove the current local room, and clear pending pairing state. Requires local/global `--yes` or global `--non-interactive`; otherwise fails rather than prompting. | `{ok: true}` |
| `room list` | Print locally stored rooms with epoch and role. | Array of `RoomRecord` values below. |
| `push [--type auto\|text\|image\|files] [--file PATH]...` | Read files, stdin, or the clipboard using the precedence below; print a summary. | `{ok: true, summary: string}` |
| `pull [--copy] [--wait] [--output DIR]` | Receive an item. Print text, adding a final newline if missing, or an image/file summary. `--copy` attempts a clipboard write. | `{kind: "text"\|"image"\|"files", summary: string}`; the plaintext and file bytes are **not** included. |
| `watch [--direction STRING]` | Run foreground sync until interrupted. The direction argument defaults to `both` but is currently ignored; configure `sync.direction` instead. | No command result; diagnostic logs only. |
| `sync pause` / `sync resume` | Persist the pause state and notify the daemon; resume may start it. The pause flag gates sending clipboard changes. | `{paused: true}` / `{paused: false}` |
| `daemon start` / `stop` / `restart` | Install/start, stop/uninstall, or restart the current user's login service. | `{daemon: "started"\|"stopped"\|"restarted"}` |
| `daemon run` | Run the daemon in the foreground; used by the installed service. | No command result; diagnostic logs only. |
| `status` | Pretty-print status below. Does not require an existing identity or room. | Status object below. |
| `devices` | Print each current-room peer's ID, name, OS, and fingerprint. Requires pairing. | Array of `PeerRecord` values below. |
| `config get [KEY]` | Pretty-print the full configuration or a single-key object. Unknown keys return `null`. | Configuration below, or `{KEY: string\|null}`. |
| `config set KEY VALUE` | Save a supported setting. Values are command-line strings; see the key table below. | `{ok: true, key: string, value: string}`; `value` echoes the argument, not a normalized stored value. |
| `doctor` | Pretty-print local identity, adapters, paths, TLS, login service, and relay-health diagnostics. | Doctor object below. A failed health check is represented inside the result. |
| `logs [--follow]` | Print the log file; `--follow` invokes `tail -n 100 -f` and requires `tail` on PATH. | Still raw log contents. If no log exists without `--follow`, emits `{ok: false, error: {message: "no logs yet"}}` and exits `0`. |

`room create --ttl` parses a duration such as `2m` and sends its whole-second
value as the room's default TTL, clamped to 10–300 seconds (default 60). It does
**not** extend the pairing code lifetime, which is fixed at 120 seconds. Current
outgoing messages still use a fixed 60-second TTL. Successful create/join starts
the login daemon unless `--no-auto-sync` or `CLIPSYNC_NO_DAEMON=1` is set.
Compatibility flags `room create -b`/`--background`/`--detach` select the existing
default background behavior and conflict with `--foreground`. The hidden
`--background-worker` flag is for the CLI's own detached worker.

For `push`, repeated `--file PATH` arguments take precedence over stdin and
`--type`; directories and symlinks are rejected. Otherwise non-terminal stdin
is read to EOF: `--type image` detects PNG/JPEG/WebP, while every other type
interprets stdin as UTF-8 text. With terminal stdin, read the OS clipboard;
`--type` does not convert or filter that item. See the protocol's size limits.
The result is a submission summary, not a peer receipt: the daemon IPC text
path currently records the item locally without immediately transmitting it.

For `pull`, a running daemon's cached item is used first; that path does not
wait or write `--output` files, and `--copy` applies only to cached text.
Otherwise a direct connection checks queued text; without `--wait`, an empty
queue fails with `no queued item` (3-second overall deadline). `--wait` waits up
to 30 seconds for text or a complete binary transfer. The direct path creates
`--output DIR` as needed and writes `clipboard.txt`, `clipboard.png`/`.jpg`/`.webp`,
or the received filenames, replacing existing same-name files. Without
`--output`, received files are staged under the local inbox; image bytes are
not printed. Clipboard write failures in `pull --copy` are currently best effort.

### Client records and configuration

| Record | Fields |
| --- | --- |
| `PeerRecord` | `device_id`, `device_name`, `os`, `public_key_hex`, `fingerprint`: strings; `role`: `owner` or `member`. |
| `RoomRecord` | `room_id`: string; `epoch`: unsigned integer; `role`: `owner` or `member`; `created_at`: time; `device_token`: secret string; `peers`: array of `PeerRecord`. |
| Status without daemon | `daemon: false`, `paused`, `connected: false`, `peer_online: false`: booleans; `room_id`, `device_id`: nullable strings; `relay_url`: string. Optional `pairing` contains `status` (`waiting`, `confirm_sas`, `paired`, `failed`), `pairing_code`, `room_id`, `expires_at`: strings; `pid`: integer; optional `error`, `sas`, `peer_device_name`, `sas_decision` (`confirmed`/`rejected`): strings. |
| Status with daemon | `daemon: true`, `paused`, `connected`, `peer_online`: booleans; `room_id`, `device_id`, `device_name`, `relay_url`: nullable strings; `last_item_kind`: nullable `text`/`image`/`files`; `last_sync_at`: nullable string (currently always `null`). |
| Doctor | `identity`, `daemon_socket`, `login_service`: booleans; `device_id`, `fingerprint`, `room`: nullable strings; `relay_url`: string; `clipboard`: nullable `{name, source: string, text, image, files: boolean, fallback: string\|null}`; `tools`: array of `[tool_name, found_boolean]`; `relay`: `{ok: boolean, status: integer}` or `{ok: false, error: string}` (a URL refused by the relay URL policy reports its error without contacting the relay); `tls`: `{verifier: "platform", ca_file: string\|null}`; `paths`: `{config_dir, data_dir, log_file: string}`. |

The full configuration has `relay_url: string`, `sync: {enabled, auto_start:
boolean, direction: "both"|"send_only"|"receive_only", types: string[],
debounce_ms, ttl_secs: unsigned integer, legacy_envelopes, strict_envelopes:
boolean}`, `notify:
{enabled, preview: boolean}`,
and `webhook: null|{url: string, secret: string}`.
[Configuration source](../crates/clipsync-storage/src/config.rs) defines these
records and defaults; [stored records](../crates/clipsync-storage/src/store.rs)
define room/peer output.

| `config get` / `set` key | Values accepted by `set`; default |
| --- | --- |
| `relay_url` | `https://` or `wss://` URL; plaintext `http://`/`ws://` only for a loopback host (`127.0.0.0/8`, `::1`, `localhost`) or with `CLIPSYNC_ALLOW_INSECURE_RELAY=1`. A URL without a scheme is rejected, as is a host not written in the canonical form the client dials (for example `0x7f.1` or `[0:0:0:0:0:0:0:1]`). The parsed, normalized URL is saved. Default `CLIPSYNC_RELAY_URL` or `https://clipsync.develicit.dev`. Relay HTTP requests do not follow redirects; plaintext relays are contacted directly, ignoring `HTTP_PROXY`/`ALL_PROXY` and system proxies, while `https://` relays honor them. |
| `sync.enabled`, `sync.auto_start`, `notify.enabled` | `1`, `true`, `yes`, `on` mean true; all other strings mean false. Default true. |
| `notify.preview` | Same boolean syntax; default false. |
| `sync.direction` | `both`, `send-only`/`send_only`, `receive-only`/`receive_only`; default `both`. Single-key `get` currently returns `sendonly`/`receiveonly`, while full JSON uses underscores. |
| `sync.ttl_secs` | Unsigned seconds, clamped to 10–300; default 60. Current sending code uses 60 regardless of this setting. |
| `sync.legacy_envelopes` | Same boolean syntax; default false. By default the envelope version is negotiated per peer (v1 until the peer proves it opens v2). True makes this device always send legacy v1 envelopes; receiving is unaffected. See [transition from v1](protocol.md#transition-from-v1). |
| `sync.strict_envelopes` | Same boolean syntax; default false. True makes this device always send v2 and reject every v1 envelope, so devices that are not updated stop syncing with it. Overrides `sync.legacy_envelopes`. |

`sync.types` (default text/image/files), `sync.debounce_ms` (default 200), and
`webhook` appear in the full config but have no `config set` key. An unknown key
or invalid direction/integer fails. `send_only` currently does not block
received clipboard writes; `receive_only` blocks the sending watch path.
Restart a running daemon after changing configuration to reload its snapshot.

### Environment and process results

| Variable | Meaning |
| --- | --- |
| `CLIPSYNC_HOME` | Root containing `config/`, `data/`, `logs/`, `inbox/`, `run/`; takes precedence over the individual path variables below. |
| `CLIPSYNC_CONFIG_DIR`, `CLIPSYNC_DATA_DIR` | Override OS-specific configuration/data directories when no `CLIPSYNC_HOME` is set. |
| `XDG_RUNTIME_DIR` | Runtime socket/PID directory when no `CLIPSYNC_HOME`; otherwise defaults to the data directory. |
| `CLIPSYNC_RELAY_URL` | Default relay when creating a configuration; does not replace an already saved `relay_url`. Use `init --relay-url` or `config set relay_url` to change it. The relay URL policy is checked when it is saved and again before every relay HTTP request or WebSocket connection, so a saved or environment URL that violates it fails at use. |
| `CLIPSYNC_ALLOW_INSECURE_RELAY` | Exactly `1` allows a plaintext `http://`/`ws://` relay on a non-loopback host; each use logs a warning. Pairing codes, pairing tokens and device tokens are then sent unencrypted. The variable must also be set for the daemon, which checks it at connect time. |
| `CLIPSYNC_CA_FILE` | Additional PEM CA certificates for TLS; invalid/unreadable bundles fail TLS configuration. |
| `CLIPSYNC_NO_DAEMON` | Exactly `1` skips automatic daemon installation/start after pairing or resume. |
| `CLIPSYNC_OS` | Device OS override: `ios`, `android`, `macos`, `linux`, `windows` (case-insensitive); otherwise native OS. |
| `CLIPSYNC_DEVICE_NAME` | Nonblank advertised device name; otherwise hostname or `clipsync-device`. |
| `CLIPSYNC_LISTEN` | Override the local daemon Unix socket or Windows named-pipe endpoint. Both daemon and CLI must use the same endpoint. |
| `RUST_LOG` | Diagnostic tracing filter; client default `warn`, relay default `info`. |

Paths and platform defaults: [storage paths](../crates/clipsync-storage/src/paths.rs).
Client exit codes currently returned by the dispatcher are:

| Code | Meaning |
| --- | --- |
| 0 | Success, including help/version. |
| 1 | General error, I/O, serialization, transfer, or unclassified storage/daemon failure. |
| 2 | Clap command/argument usage error, or a relay URL refused by the transport policy (plaintext non-loopback or missing scheme). |
| 3 | Pairing failure. |
| 4 | Not initialized or not paired. |
| 5 | Transport or HTTP error reaching the dispatcher as that error type. |
| 6 | Cryptographic error. |
| 7 | Clipboard/adaptor error, including client item validation. |
| 10 | Timeout. |

Codes 8 (`PeerOffline`) and 9 (`TooLarge`) are declared internally but are not
currently selected by `CoreError::exit_code`; do not depend on those values.
Some operations wrap network failures as general errors, so messages provide
additional detail beyond the exit category.

## Relay service and HTTP API

The `clipsync-relay` command accepts `--bind SOCKET_ADDRESS` and
`--public-url URL`, with corresponding `CLIPSYNC_RELAY_BIND` and
`CLIPSYNC_RELAY_PUBLIC_URL` defaults. CLI values take precedence over those
environment settings. `--help`/`--version` exit successfully; argument errors
exit with `2`, and startup failures are nonzero. Diagnostics describe startup
and requests. See [self-hosting](self-hosting.md) for deployment variables and
TLS termination. The relay holds rooms, pairing sessions, and queued text in
memory; restarting loses that state, even with a fixed token secret.

`CLIPSYNC_RELAY_TOKEN_SECRET` supplies the HMAC signing key as UTF-8 string
bytes, without trimming or hex decoding. Unset, empty, or whitespace-only
values generate a fresh random 32-byte key; an explicit nonblank value shorter
than 32 bytes prevents startup without disclosing the value. Compose forwards
the optional setting and has no shared default key. See
[token rotation and re-pairing](self-hosting.md#token-keys-and-rotation).
Webhook URL/secret settings are optional and must both be present to emit the
webhook output described below. Without an explicit public URL, it is derived from the
environment bind port (default `http://127.0.0.1:7600`); `--bind` alone does not
recompute it. Current pairing responses advertise the relative `ws_path`, not
that URL. A bind port of `0` (for example `--bind 127.0.0.1:0`) lets the
operating system choose a free port. Pass `--public-url` explicitly in that
case, because a derived URL would carry port `0`. The startup diagnostic
`clipsync-relay listening ... addr=ADDRESS` is written to standard output once
the socket is bound and reports the chosen address. It is an info-level
tracing event, so a `RUST_LOG` filter above `info` suppresses it, and terminal
colour can change its formatting; set `NO_COLOR=1` when a script parses it.

HTTP definitions: [router and handlers](../crates/clipsync-relay/src/lib.rs),
[pairing behavior](../crates/clipsync-relay/src/state.rs), and
[request/response records](../crates/clipsync-protocol/src/messages.rs).
Pairing endpoints use `Content-Type: application/json` and return `200` JSON on
success. They do not require an Authorization header. The HTTP request-body
limit is 393,216 bytes (384 KiB).

| Endpoint | Request | Success response |
| --- | --- | --- |
| `GET /healthz` | No body or authentication. | `200`, JSON `{ok: true, service: "clipsync-relay"}`. |
| `GET /metrics` | Disabled unless the operator sets `CLIPSYNC_RELAY_METRICS_TOKEN` or `CLIPSYNC_RELAY_METRICS_TOKEN_FILE`; disabled returns `404`. When enabled, requires `Authorization: Bearer <token>`; a missing, malformed, or wrong token returns `401` with `WWW-Authenticate: Bearer`. | `200`, plain text metric lines: `clipsync_ws_connections` (sockets past a valid `hello`, pairing or room member), `clipsync_frames_forwarded`, `clipsync_pairing_ok`, `clipsync_pairing_fail`, `clipsync_pairing_throttled` (joins refused because a failed-join budget was spent), `clipsync_expired`, `clipsync_ws_sockets` (open WebSocket connections holding a connection slot, authenticated or not), `clipsync_ws_refused` (upgrades refused by a connection cap), `clipsync_pairing_sessions` (pairing sessions held), `clipsync_pairing_create_throttled` (creations refused by a creation budget), `clipsync_pairing_create_full` (creations refused by the session cap, or by a room cap where every room has a device online), `clipsync_rooms`, `clipsync_queue_full` (offline text envelopes not queued because the queue, or the sender's share of it, was full), `clipsync_rooms_evicted` (idle rooms evicted at the room cap), each followed by a numeric value and newline. |
| `POST /v1/pairing/create` | `{pairing_session_id, device_id, device_name, os: string, protocol_version: u16, ttl_secs?: u64\|null}`. Version must be `1`. | `{room_id, pairing_code, pairing_token: string, expires_at: time, ws_path: "/v1/ws"}`. Pairing code is a six-character decimal string, valid for 120 seconds. `ttl_secs` defaults to 60 and is clamped to 10–300 as the room's stored default TTL. `device_name` and `os` are at most 256 bytes each (the same limit applies to join fields); `pairing_session_id` and `device_id` follow the ID rule above. |
| `POST /v1/pairing/join` | `{pairing_code, device_id, device_name, os: string, protocol_version: u16}`. Version must be `1`; joiner must be a different device. | `{pairing_session_id, room_id, pairing_token: string, peer: PeerInfo, expires_at: time, ws_path: "/v1/ws"}`. `peer` describes the creator. One joiner per session. |
| `GET /v1/ws[?token=TOKEN]` | Standard WebSocket upgrade; optional query token. Authentication occurs in the `hello` text frame after upgrade. | `101 Switching Protocols`, then the messages below. Refused before the upgrade when a connection cap is reached (see the error table). The relay closes a socket that sends no valid `hello` within 10 seconds, or that is silent for 60 seconds after `hello`. Before `hello`, a socket may also be closed to admit a newer one once unauthenticated sockets hold their share of the connection slots, or the relay holds its maximum of connections. |

Handler-generated HTTP failures have the JSON shape
`{"code":"code_invalid","message":"unknown pairing code"}`:

| Endpoint / HTTP status | `code` | Meaning |
| --- | --- | --- |
| create or join / 400 | `protocol_version` | Unsupported protocol version. |
| create or join / 400 | `invalid_request` | Body is not valid JSON or does not match the request type, including an ID that fails the rule above. The message is fixed and does not quote the input. |
| create / 400 | `pairing_create` | Invalid creation request, for example a `device_name` or `os` longer than 256 bytes. |
| create / 429 | `too_many_pairings` | The client's or the relay-wide pairing-creation budget is spent. |
| create / 503 | `relay_full` | The relay holds its maximum of open pairing sessions, or every room at the room cap has a device online. |
| ws / 429 | `too_many_connections` | The client address, or its IPv6 /48, holds its maximum of WebSocket connections. |
| ws / 503 | `relay_full` | The relay holds its maximum of WebSocket connections. |
| join / 404 | `code_invalid` | Unknown code, including expired sessions already removed by cleanup. |
| join / 410 | `code_expired` | Session exists but has expired. |
| join / 429 | `too_many_attempts` | Pairing attempt limit reached: the session's attempts, the client's failed joins, or the relay-wide failed-join budget. Returned even for a correct code once a failure budget is spent. |
| join / 409 | `busy` | Session already has a joiner or is completed. |
| join / 400 | `join_failed` | Other join rejection, including using the creator's device ID. |

A missing/wrong content type, oversized bodies,
unknown routes, and failed upgrade extraction can be rejected by the HTTP
framework/body limiter before a handler runs. Their response bodies need not
use this JSON error shape; clients must check the status before decoding.

## WebSocket v1

Definitions: [control schemas](../crates/clipsync-protocol/src/messages.rs),
[relay dispatch](../crates/clipsync-relay/src/ws.rs),
[token verification](../crates/clipsync-relay/src/auth.rs), and
[chunk codec](../crates/clipsync-protocol/src/chunk.rs).

Each text frame is one JSON object with a snake_case `type`. The frame/message
limit is 384 KiB. Send `hello` before authenticated routing messages. Supply
the secret token in `hello.token`; if this is an empty string, the relay uses
the optional `token` query parameter. An HTTP `Authorization` header is not an
alternative. Prefer the frame token to avoid query-string logging. Pairing
HTTP responses supply short-lived pairing tokens; `pairing_complete` supplies
device tokens (issued for 365 days). Treat tokens as opaque credentials, not
JWTs. The relay verifies their signature, expiry, device, optional room, and
stored membership/revocation for device-token connections. A successful
upgrade alone does not authenticate a connection.

Types in the next table: `string` = JSON string; `u16`/`u32`/`u64` = unsigned
integer of that width; `i64` = signed 64-bit integer; `?` = nullable/omittable
field. `DeviceRole` is `owner` or `member`; `ClientRole` is `pairing_a`,
`pairing_b`, or `member`. Relay-issued optional fields are normally present
as `null` when unset.

| `type` | Fields besides `type` | Direction / purpose |
| --- | --- | --- |
| `hello` | `protocol_version: u16` (=1), `device_id`, `device_name`, `os`, `token: string`; `role: ClientRole`; `room_id?: string`, `pairing_session_id?: string`. | Client → relay: establish identity and pairing/member connection. |
| `hello_ok` | `room_id: string`, `peer_online: boolean`, `peers: PeerInfo[]`, `queued_text?: QueuedEnvelope`, `device_token?: string`. | Relay → client. Pairing connections get empty peers/queue; member connections may receive queued text. `device_token` is currently null here. |
| `peer_joined` | `device: PeerInfo`. | Relay → peer: device connected. |
| `peer_left` | `device_id: string`. | Relay → peer: device disconnected. |
| `spake2_msg` | `pairing_session_id`, `message: string`. | Client ↔ peer via relay: pairing exchange. |
| `key_confirm` | `pairing_session_id`, `mac: string`. | Client ↔ peer: pairing key confirmation. |
| `device_bind` | `pairing_session_id`, `nonce`, `ciphertext: string`. | Client ↔ peer: encrypted device binding. |
| `pairing_complete` | `room_id`, `device_token: string`, `epoch: u64`, `role: DeviceRole`. | Relay → each paired client: persist room and token; initial epoch is 1. |
| `noise_handshake` | `message: string`. | Client ↔ peer: reconnect handshake; carries the envelope capability payload ([protocol](protocol.md#reconnect)). |
| `text_envelope` | `routing: RoutingHeader`, `nonce`, `ciphertext: string`. | Client → relay → peers; offline peers retain the latest unexpired envelope. |
| `manifest` | `routing: RoutingHeader`, `nonce`, `ciphertext: string`, `chunk_count: u32`, `total_bytes: u64`. | Client → online peers: encrypted binary-transfer metadata. |
| `ack` | `message_id: string`, `transfer_id?: string`, `status: AckStatus`, `detail?: string`. | Relay or peer → sender: transfer status. |
| `epoch_wrap_msg` | `epoch: u64`, `wraps: EpochWrap[]`. | Client → peers: encrypted epoch-key updates. |
| `revoke` | `device_id: string`. | Owner → relay/peers: revoke a room device and close its connection. |
| `ping` / `pong` | `ts: i64`. | Application heartbeat; relay echoes `ping.ts` in `pong`. |
| `error` | `code`, `message: string`. | Relay → client: failure; client → relay also aborts its pending pairing. |

`AckStatus` values are `accepted`, `streaming`, `delivered`, `expired`,
`rejected`, `too_large`, `peer_offline`. For text, relay `accepted` means queued
for offline peers, `peer_offline` with a `detail` means no peer was online and
the relay's offline queue, or the sender's share of it, was full, and `delivered` means forwarded to at least one online
connection; neither proves a clipboard write. A binary manifest gets
`streaming` when an online peer exists, otherwise `peer_offline`. Binary
frames with no peer receive `peer_offline` with `message_id: "chunk"` and a
null transfer ID. Peers can emit further acknowledgements.

| Nested record | Fields / representation |
| --- | --- |
| `PeerInfo` | `device_id`, `device_name`, `os: string`; `role: DeviceRole`; `online: boolean`. |
| `RoutingHeader` | `version: u16` (envelope version: 1 legacy, 2 authenticated; see [protocol](protocol.md#envelope-versions)); `message_id`, `transfer_id`, `room_id`, `sender_device_id: string`; `recipient`: `"broadcast"` or `{"device":"DEVICE_ID"}`; `sent_at`, `expires_at: time`; `item_kind`: `text`/`image`/`files`; `seq: u64`. |
| `QueuedEnvelope` | `routing: RoutingHeader`, `nonce`, `ciphertext: string`. |
| `EpochWrap` | `device_id`, `eph_public`, `nonce`, `ciphertext: string`. |

The core encodes `spake2_msg.message` and `key_confirm.mac` as standard Base64;
Noise handshake messages, nonces, and ciphertexts use hex strings.
`manifest.ciphertext` encrypts a `ManifestPlain` record: `item_kind`,
`total_bytes: u64`, `chunk_count: u32`, `content_hash: string`, `files:
FileManifest[]`, `image?: ImageManifest`, `text_chars?: unsigned integer`.
`FileManifest` contains `name: string`, `size: u64`, `mime?: string`, and
`hash: string`; `ImageManifest` contains `mime: string`, `size: u64`,
`width?: u32`, `height?: u32`, and `hash: string`. The encrypted
`DeviceBindPlain` contains `device_id`, `static_public`, `device_name`, `os`,
`fingerprint`, `epoch_key: string`, `epoch: u64`, and `role: DeviceRole`.
These plaintext records are for endpoints; the relay forwards ciphertext.
Binary chunks use the layout in [the protocol](protocol.md#binary-chunk-frames),
with a 12-byte nonce preceding the authenticated ciphertext.

Relay WebSocket failures are `error` messages with `code: "error"` and a
human-readable message, for example unsupported version, malformed/expired
token, token device/room mismatch, unknown room/device, revoked device,
`hello required`, item-kind mismatch, or missing owner permission. Code
`rate_limited` reports an excessive message burst and closes the connection;
`too_large` is the explicit binary-size rejection, although transport size
limits can close the connection first. Invalid control-message JSON is logged
and ignored without a structured error. Treat a disconnect or timeout as a
possible failure even when no error frame arrives.

### Optional relay webhook output

When both `CLIPSYNC_RELAY_WEBHOOK_URL` and
`CLIPSYNC_RELAY_WEBHOOK_SECRET` are set, the relay sends JSON metadata via HTTP
POST. [Webhook source](../crates/clipsync-relay/src/webhook.rs) defines signing
and destination checks; [relay dispatch](../crates/clipsync-relay/src/ws.rs)
defines emitted events. Event objects share `type`, `room_id`, `device_id`,
`status: "ok"`, and `ts: time`:

| `type` | Additional fields |
| --- | --- |
| `device.joined` | `item_kind: null`. Emitted for a member-token connection. |
| `clip.received` | `item_kind: "text"`, `encrypted_size: integer` (hex ciphertext string length). This records relay acceptance, not a recipient clipboard write. |
| `room.revoked` | None; `device_id` is the revoked device. |

Headers are `X-ClipSync-Timestamp` (Unix seconds), `X-ClipSync-Event-Id` (ULID),
and `X-ClipSync-Signature: sha256=HEX`. Verify HMAC-SHA-256 with the webhook
secret over the exact bytes `timestamp + "." + event_id + "." + body`; apply
your own replay/freshness checks. Requests time out after five seconds, do not
follow redirects, and currently have no retry or HTTP success-status check.

## Mobile UniFFI API

Definitions: [exported methods, records, errors, and clipboard callbacks](../crates/clipsync-mobile/src/lib.rs).
Swift/Kotlin names follow the generated bindings; the names/types below use
the Rust interface. `Result<T, MobileError>` becomes a throwing/error-returning
operation in the host binding. The only exported error variant is
`MobileError::Message(string)`; messages describe storage, network, pairing,
runtime, or thread-start failures and are not stable machine error codes.

| Method | Inputs | Result and behavior |
| --- | --- | --- |
| `ClipSyncClient::new` | `data_dir`, `relay_url`, `os`, `device_name`: strings. | Shared client object. Creates app directories and a runtime; attempts identity/config initialization with the supplied relay. `data_dir` is the root layout described under `CLIPSYNC_HOME`. Empty OS/name leave existing overrides alone. Fails if initialization fails, including a `relay_url` refused by the relay URL policy described under `relay_url`. |
| `status()` | None. | `MobileStatus {device_id, fingerprint, relay_url, os: string, room_id: string?}`. Creates/reuses identity; an unpaired client has a null room ID. |
| `create_room_verified(verifier)` | Host implementation of `SasVerifier`. | `PairingOffer {pairing_code, room_id, expires_at: string}`; expiry is RFC 3339. Returns after the offer is available and waits for the joiner in a background task, which calls `verifier.confirm_sas` before anything is sent or stored. The offer alone does not mean pairing completed; poll until `status().room_id` equals the offered room ID. Later pairing failures, including a rejected code, are logged rather than returned from this completed call. |
| `join_room_verified(code, verifier)` | Six-digit code string, preserving leading zeros; `SasVerifier`. | Room ID string after the handshake completes, `verifier` confirmed, and state is saved. A rejected code returns an error and stores nothing. |
| `create_room()` | None. | Same as `create_room_verified` but accepts without comparing verification codes. Kept so existing apps keep working; it cannot detect a malicious relay or a guessed code. |
| `join_room(code)` | Six-digit code string, preserving leading zeros. | Same as `join_room_verified` but accepts without comparing verification codes. Kept for existing apps; same limitation. |
| `start_sync(clipboard)` | Host implementation of `ClipboardBridge`. | Returns unit after starting a background thread; repeated calls on the same client return success without starting another loop. Requires paired state for useful sync. Subsequent loop failures are logged, not delivered to the caller. No stop/pause/dispose-sync method is exported. |

These are synchronous FFI methods: network operations in create/join block the
calling thread until their result is available; call them off the UI thread.
Keep the client and clipboard bridge alive for the sync session. Constructor
OS/name/home settings are process-wide environment overrides, and OS detection
is cached; use one active identity/client configuration per process rather
than assuming independent instances. Neither pairing method installs an OS
login service; native apps call `start_sync` and manage platform lifecycle.

`SasVerifier.confirm_sas(sas, peer_device_name) -> bool` is called once per
pairing on a background thread, after key confirmation. Show the six-digit
`sas` (for example as `482 913`) and the relay-reported `peer_device_name`
(fixed before SPAKE2 starts; control, bidi, blank and invisible characters
escaped; at most 40 terminal columns; show it in its own text element, apart
from the code), ask
whether the other device shows the same code, block until the user answers,
and return `true` only for a match. Returning `false` aborts the pairing and
stores nothing. The pairing deadline is 120 seconds from the offer, not from
the prompt. If the pairing ends before the user answers (deadline, the other
device rejected, relay error), `SasVerifier.sas_cancelled()` is called: dismiss
the dialog and let the pending `confirm_sas` return; its answer is ignored.
`sas_cancelled` must not block. Marshal the dialog to the UI thread; do not
block the UI thread waiting for it. Keep the client alive until the dialog is
dismissed, because dropping it waits for a blocked `confirm_sas`. See
[security](security.md#verification-code-sas).

Implement every callback in `ClipboardBridge`; it is a thread-safe
(`Send + Sync`) interface called by the background sync runtime. Marshal any
UI- or OS-main-thread-only work in the host app:

| Callback | Input / output contract |
| --- | --- |
| `read_text()` | Return optional string; null or empty means no text. |
| `write_text(text)` | Accept received text string; return unit. |
| `read_image_png()` | Return optional byte array containing a PNG image; null or 16 bytes or fewer is treated as no image. An available image takes precedence over text. |
| `write_image_png(png)` | Accept received encoded image bytes; return unit. Despite the callback name, the core currently forwards received image/file bytes without transcoding, so decode/check the actual image format. |
| `notify(title, body)` | Accept notification strings; return unit. Also used for a received non-image file notification. |

The bridge advertises text/image/watch support and no file-selection support.
On incoming file items only the first file is handled: an image MIME type is
sent to `write_image_png`; other files trigger a notification. There is no
exported file-read callback or file-save result. Callback writes return no
error value, so host clipboard failures cannot be returned through this API.

## Credential redaction and local storage migration

Current `room list` / `room join` JSON omits `device_token`, and configuration
JSON omits webhook `secret`. This is intentional credential redaction. Rust
callers still receive the in-memory fields where authentication is required.
Loading legacy state/config writes separate private credentials before replacing
metadata; initialization propagates migration/parse failures and preserves the
original configuration. Back up the complete private data directory before
upgrading. Older binaries cannot read the migrated layout: update both peers and
re-pair instead of downgrading migrated state. See [security](security.md).
