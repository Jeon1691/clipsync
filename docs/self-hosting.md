# Self-hosted relay

Hosted origin used by this project: `https://clipsync.develicit.dev`
(Proxmox + Cloudflare Tunnel; see `deploy/proxmox/`).

## Upgrade to v0.2.0: choose the origin transport

Starting with v0.2.0, the relay refuses to listen unless it has both TLS
certificate/key files or an explicit `CLIPSYNC_RELAY_ALLOW_HTTP=true` opt-in.
The default advertised URL uses HTTPS. Old commands that started a plaintext
listener without transport settings now fail before binding a port.

- For direct HTTPS/WSS, provide both TLS files and an HTTPS public URL, as shown
  below. Keep `CLIPSYNC_RELAY_ALLOW_HTTP` unset or `false`.
- For the existing Proxmox + Cloudflare Tunnel deployment, use the updated
  `deploy/proxmox/docker-compose.yml`. It explicitly enables HTTP for its
  loopback-only origin; Cloudflare continues to terminate public HTTPS/WSS.
  Preserve the private `.env`, token key and hostname during the upgrade.
- For another TLS proxy, explicitly opt into HTTP and restrict access to the
  proxy/local host. Public clients must still use the proxy's HTTPS URL.

The root Compose example explicitly opts into local-development HTTP and only
publishes port 7600 on host loopback. It is ready for `http://127.0.0.1:7600`.
Clients accept plaintext relay URLs only for loopback hosts; a client on another
machine must use the relay's HTTPS/WSS URL. `CLIPSYNC_ALLOW_INSECURE_RELAY=1` on
the client overrides this for a fully trusted network, with a warning, but
exposes pairing codes and device tokens to anyone on the path.
Binding a raw relay publicly now requires a deliberate transport choice.

## Direct HTTPS/WSS

Obtain a certificate from your chosen CA. Configure a PEM chain with the leaf
certificate first and a matching unencrypted PEM private key:

```bash
export CLIPSYNC_RELAY_BIND=0.0.0.0:7600
export CLIPSYNC_RELAY_PUBLIC_URL=https://relay.example.com:7600
export CLIPSYNC_RELAY_TLS_CERT_FILE=/etc/clipsync/tls/fullchain.pem
export CLIPSYNC_RELAY_TLS_KEY_FILE=/etc/clipsync/tls/key.pem
clipsync-relay
```

The CLI equivalents are `--tls-cert-file`, `--tls-key-file`, `--public-url` and
`--bind`. TLS certificate and key settings must be supplied together; invalid,
unreadable, oversized or mismatched files stop startup. TLS and `--allow-http`
cannot be combined. Certificates load at startup; renew them through your
normal certificate manager and restart the relay after replacement.

The rustls listener permits only TLS 1.2 and 1.3. It uses the maintained
[axum-server RustlsAcceptor](https://docs.rs/axum-server/0.9.2/axum_server/tls_rustls/struct.RustlsAcceptor.html),
with a five-second TLS handshake deadline and at most 128 incomplete
handshakes. Excess attempts close immediately, without a waiting socket queue.
The server also limits the time to its first HTTP request to ten seconds. The
HTTP/WebSocket router still receives the actual TCP peer address for rate
limits. By default, proxy deployments see the immediate proxy's address, so all
clients behind one proxy share one client's pairing budgets and connection cap
(32 WebSocket connections by default), and forwarded-IP headers are ignored.

To give clients behind a reverse proxy separate pairing budgets and connection
caps, set
`CLIPSYNC_RELAY_TRUSTED_PROXIES` (or `--trusted-proxies`) to a comma-separated
list of the proxy addresses or CIDR ranges, for example `127.0.0.1` for the
nginx example below. The relay then reads `X-Forwarded-For` only on connections
from those addresses, and uses the rightmost entry that is not itself a trusted
proxy. Entries to the left of that entry are ignored because clients can write
them. Configure the proxy to append the connecting address, as nginx's
`$proxy_add_x_forwarded_for` and Caddy's `reverse_proxy` do. List only addresses
that untrusted clients cannot connect from. A process that can connect from a
trusted address can claim any client address. The relay logs a startup warning
when an entry covers a whole address family (`0.0.0.0/0`, `::/0`). IPv4-mapped
IPv6 entries such as `::ffff:127.0.0.1` match the same IPv4 peers and hops as
`127.0.0.1`. An IPv6 range that contains all of `::ffff:0:0/96` also trusts
every IPv4 address. Empty entries are ignored, so an empty value or a trailing
comma leaves the setting unset. Give the whole list as one value; repeating
`--trusted-proxies` is rejected at startup. The setting affects the per-client
pairing-join and pairing-creation budgets and the per-client connection cap
only. Leave it unset when clients connect to the relay directly.

For a container, mount the certificate directory read-only and pass the paths
at runtime. Keep private files out of the image and build context. This example
uses an already verified official image digest:

```bash
docker run -d --name clipsync-relay --restart unless-stopped \
  --read-only --cap-drop ALL --security-opt no-new-privileges \
  -p 7600:7600 \
  --mount type=bind,src=/etc/clipsync/tls,dst=/run/tls,readonly \
  -e CLIPSYNC_RELAY_TLS_CERT_FILE=/run/tls/fullchain.pem \
  -e CLIPSYNC_RELAY_TLS_KEY_FILE=/run/tls/key.pem \
  -e CLIPSYNC_RELAY_PUBLIC_URL=https://relay.example.com:7600 \
  ghcr.io/jeon1691/clipsync-relay@sha256:VERIFIED_INDEX_DIGEST
```

The container runs as UID/GID 65532. Its user must be able to traverse the
mounted directory and read both files. On Unix the relay rejects private keys
and token-secret files that are group-writable or accessible to other users;
use `0600` for its own files or `0640` with a deliberately selected reader group.
For a root-owned container secret, assign the reader group GID 65532, make the
directory traversable by that group, and use `0640` on the key. A root-only file
that UID/GID 65532 cannot read causes startup to fail; do not weaken its mode to
`0644`. On Windows, configure equivalent file ACLs for the relay service user.

Clients verify issuer and hostname normally. Publicly trusted certificates need
no client override; for a private CA, supply its public root PEM through
`CLIPSYNC_CA_FILE`. This extends trust and preserves hostname/issuer validation.
Never distribute the server private key to clients.

## Cloudflare Tunnel

`cloudflared` connects to the relay from the tunnel host, so without trusted
proxies every visitor shares one pairing-join budget: one guesser, or a few
mistyped codes from different users, refuses every pairing until the window
ends. Visitors would also share one client's creation budget and its 32
WebSocket connections. Cloudflare appends the address it received each request from to
`X-Forwarded-For`, keeping any value the visitor sent, and `cloudflared`
forwards that header to the origin. Trust only the address `cloudflared`
connects from, and the relay uses that appended visitor address:

- `cloudflared` on the Docker host, relay port published on host loopback (the
  [Proxmox manifest](../deploy/proxmox/docker-compose.yml)): connections reach
  the container from its network gateway. Pin the network's subnet and gateway
  and trust the gateway, as the manifest does with `10.207.76.1`. If you change
  the subnet, change `CLIPSYNC_RELAY_TRUSTED_PROXIES` with it.
- `cloudflared` in a container on the same Compose network: give it a fixed
  address on that network and trust that address.
- `cloudflared` on the relay host without Docker: trust `127.0.0.1`.

The relay does not read `CF-Connecting-IP` or other Cloudflare-specific headers.
`X-Forwarded-For` is enough, and like every forwarded header it is honored only
on connections from a trusted proxy. Two zone settings change what the relay
sees. The *Remove visitor IP headers* Managed Transform drops the visitor
address, so all visitors share the gateway's budget again. *Pseudo IPv4:
Overwrite Headers* replaces IPv6 visitor addresses with pseudo IPv4 addresses
in `X-Forwarded-For`; IPv6 visitors are then counted by that pseudo IPv4
address instead of by their /64 prefix. Do not publish the relay port beyond
host loopback: any client that can connect from a trusted address can claim any
client address.

## Pairing-join limits

The default budgets suit a small self-hosted relay. To tune them, set:

| Variable | Default | Allowed values |
| --- | ---: | --- |
| `CLIPSYNC_RELAY_PAIRING_CLIENT_FAIL_LIMIT` | `10` | 1 to the relay-wide limit; failed joins per client address (IPv6: per /64) per client window |
| `CLIPSYNC_RELAY_PAIRING_CLIENT_FAIL_WINDOW_SECS` | `900` | `120` (one pairing TTL) to `86400` |
| `CLIPSYNC_RELAY_PAIRING_RELAY_FAIL_LIMIT` | `20` | 1 to `50`; failed joins across all clients per 120-second pairing TTL |

The CLI equivalents are `--pairing-client-fail-limit`,
`--pairing-client-fail-window-secs` and `--pairing-relay-fail-limit`. Values
outside these ranges stop startup. The relay-wide cap of 50 is the security
bound: a pairing code can be tested at most twice that many times while it is
live, so the chance of guessing one session stays at or below 10^-4. Raising the
limits trades that margin for fewer lockouts. Lowering them does the opposite.
Watch `clipsync_pairing_throttled` on `/metrics` to see how often joins are
refused.

## Resource limits

`/v1/pairing/create` and `/v1/ws` need no credentials, so the relay bounds what
they can allocate. The defaults suit a small self-hosted relay:

| Variable | Default | Allowed values |
| --- | ---: | --- |
| `CLIPSYNC_RELAY_PAIRING_CLIENT_CREATE_LIMIT` | `20` | 1 to the relay-wide creation limit; pairings one client address (IPv6: one /64) may create per client creation window |
| `CLIPSYNC_RELAY_PAIRING_CLIENT_CREATE_WINDOW_SECS` | `900` | `120` (one pairing TTL) to `86400` |
| `CLIPSYNC_RELAY_PAIRING_RELAY_CREATE_LIMIT` | `100` | 1 to `500`; pairings all clients together may create per 120-second pairing TTL |
| `CLIPSYNC_RELAY_MAX_ROOMS` | `10000` | more than `CLIPSYNC_RELAY_MAX_CONNECTIONS` plus 1,000; rooms held at once |
| `CLIPSYNC_RELAY_MAX_CONNECTIONS` | `512` | at least 1; WebSocket connections at once |
| `CLIPSYNC_RELAY_MAX_CLIENT_CONNECTIONS` | `32` | 1 to `CLIPSYNC_RELAY_MAX_CONNECTIONS`; WebSocket connections from one client address (IPv6: one /64) at once |

The CLI equivalents are `--pairing-client-create-limit`,
`--pairing-client-create-window-secs`, `--pairing-relay-create-limit`,
`--max-rooms`, `--max-connections` and `--max-client-connections`. Values
outside these ranges stop startup.

Each IPv6 /48 gets three clients' worth of the per-client creation limit and
connection cap, so one site cannot multiply its share by spreading over many
/64s. A spent creation budget answers `429 too_many_pairings`. The relay also
holds at most 1,000 open pairing sessions, a fixed 0.1% of the 10^6 pairing
codes, so a new session always gets a fresh code. At that cap, creation answers
`503 relay_full`. The relay-wide creation limit stops at 500 so that creations
within the limit cannot fill the session cap on their own. Connections past a
cap are refused before the WebSocket upgrade, with `429 too_many_connections`
for one client or site, or `503 relay_full` for the whole relay. Sockets that
are not room members yet (before `hello`, or while pairing) may hold 75% of
the connection slots. Past that, or at the relay-wide cap while any of them is
open, a new socket evicts the oldest of them instead. Evicted sockets close
within a fraction of a second; until then they may take up to 5% more than
`CLIPSYNC_RELAY_MAX_CONNECTIONS` (at least one).

A /48 is not always one site. Some ISPs hand out /56s, or one /64 per mobile
device, from a shared /48, so their customers share one /48's creation and
connection budget, and one of them can spend it for its neighbours. If many
of your users sit behind such a provider, raise
`CLIPSYNC_RELAY_PAIRING_CLIENT_CREATE_LIMIT` and
`CLIPSYNC_RELAY_MAX_CLIENT_CONNECTIONS`, which scale the /48 budgets.

Each connection takes one file descriptor, so `CLIPSYNC_RELAY_MAX_CONNECTIONS`
must fit under the relay process's open-file soft limit, with room for the
listener, up to 128 TLS handshakes, webhooks and the 5% of closing sockets. Linux and systemd services
default to 1024, which fits the default of 512. A shell on macOS defaults to
256, so before starting the relay from one, run `ulimit -n 1024` or lower
`CLIPSYNC_RELAY_MAX_CONNECTIONS`. Raise the limit (for example `LimitNOFILE=`
in a systemd unit) before raising `CLIPSYNC_RELAY_MAX_CONNECTIONS`.

Open sessions that could still complete count against
`CLIPSYNC_RELAY_MAX_ROOMS`. The room cap never blocks pairing: at the cap, a
new pairing evicts the least recently used room that has no device online. A
room is used when one of its devices authenticates, sends anything, or
disconnects. Rooms with a device online are never evicted. Startup requires the
cap to exceed `CLIPSYNC_RELAY_MAX_CONNECTIONS` plus the 1,000 open sessions, so
there is always a room to evict. An evicted room's devices get `unknown room`
when they reconnect and must pair again, the same outcome as a relay restart,
because rooms are held in memory. Size `CLIPSYNC_RELAY_MAX_ROOMS` well above
the rooms your users keep, so that only long-unused rooms are evicted. Watch
`clipsync_rooms` and `clipsync_rooms_evicted`. Otherwise rooms persist until
every device in a room has been revoked, or until its device tokens expire,
one year after pairing, with none of its devices connected.

A WebSocket that sends no valid `hello` within 10 seconds of the upgrade, or
that goes silent for 60 seconds after its `hello` (a room member or a pairing
socket), is closed and frees its connection slot. Offline text is queued for
at most 256 recipients at once, at most 16 of them from one client address,
each envelope for at most 60 seconds.
`/metrics` (when enabled; see [Metrics](#metrics)) reports
`clipsync_ws_sockets`, `clipsync_ws_refused`,
`clipsync_pairing_sessions`, `clipsync_pairing_create_throttled`,
`clipsync_pairing_create_full`, `clipsync_rooms`, `clipsync_queue_full` and
`clipsync_rooms_evicted`.

## Production (Proxmox)

The live relay runs on the Proxmox host at `/opt/clipsync`:

- container `clipsync-relay` bound to `127.0.0.1:7600`
- Cloudflare Tunnel hostname `clipsync.develicit.dev` → that origin
- public URL `https://clipsync.develicit.dev`
- Compose network pinned to `10.207.76.0/29`, with its gateway `10.207.76.1`
  as the only trusted proxy (see [Cloudflare Tunnel](#cloudflare-tunnel))

Applying this manifest to a running deployment recreates the container on the
pinned network. That restart loses in-memory rooms and pairing sessions.

```bash
ssh proxmox
cd /opt/clipsync/deploy/proxmox
docker compose ps
docker compose logs -f --tail=100
curl -fsS http://127.0.0.1:7600/healthz
```

Rebuild after a source sync:

```bash
rsync -az --exclude target --exclude .git --exclude deploy/proxmox/.env ./ proxmox:/opt/clipsync/
ssh proxmox 'cd /opt/clipsync/deploy/proxmox && docker compose up -d --build'
```

## Docker

```bash
docker compose up --build
```

The example explicitly enables HTTP inside its container, reachable only at
`127.0.0.1:7600` on the host. For a public reverse-proxy deployment, terminate
TLS with Caddy or nginx and keep `CLIPSYNC_RELAY_PUBLIC_URL` set to the public
HTTPS URL. The following nginx location assumes a separately configured TLS
server block:

```nginx
location / {
    proxy_pass http://127.0.0.1:7600;
    proxy_http_version 1.1;
    proxy_set_header Upgrade $http_upgrade;
    proxy_set_header Connection "upgrade";
    proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
    proxy_read_timeout 3600;
}
```

## Environment

| Variable | Meaning |
| --- | --- |
| `CLIPSYNC_RELAY_BIND` | Listen address, default `0.0.0.0:7600` |
| `CLIPSYNC_RELAY_PUBLIC_URL` | URL clients should use; HTTPS by default |
| `CLIPSYNC_RELAY_TLS_CERT_FILE` | PEM certificate chain for direct HTTPS/WSS |
| `CLIPSYNC_RELAY_TLS_KEY_FILE` | Matching PEM private key; both TLS files are required |
| `CLIPSYNC_RELAY_ALLOW_HTTP` | Explicit `true` for a local/TLS-proxy plaintext origin; default `false` |
| `CLIPSYNC_RELAY_TRUSTED_PROXIES` | Optional comma-separated proxy addresses/CIDRs whose `X-Forwarded-For` identifies clients for per-client pairing limits and connection caps; unset or empty uses only the TCP peer address |
| `CLIPSYNC_RELAY_PAIRING_CLIENT_FAIL_LIMIT` | Failed pairing joins per client per window; default `10` (see [pairing-join limits](#pairing-join-limits)) |
| `CLIPSYNC_RELAY_PAIRING_CLIENT_FAIL_WINDOW_SECS` | Per-client window in seconds; default `900` |
| `CLIPSYNC_RELAY_PAIRING_RELAY_FAIL_LIMIT` | Failed pairing joins relay-wide per pairing TTL; default `20`, at most `50` |
| `CLIPSYNC_RELAY_PAIRING_CLIENT_CREATE_LIMIT` | Pairings one client may create per window; default `20` (see [resource limits](#resource-limits)) |
| `CLIPSYNC_RELAY_PAIRING_CLIENT_CREATE_WINDOW_SECS` | Per-client creation window in seconds; default `900` |
| `CLIPSYNC_RELAY_PAIRING_RELAY_CREATE_LIMIT` | Pairings created relay-wide per pairing TTL; default `100`, at most `500` |
| `CLIPSYNC_RELAY_MAX_ROOMS` | Rooms held at once before the least recently used idle room is evicted; default `10000` |
| `CLIPSYNC_RELAY_MAX_CONNECTIONS` | WebSocket connections at once; default `512` |
| `CLIPSYNC_RELAY_MAX_CLIENT_CONNECTIONS` | WebSocket connections per client address at once; default `32` |
| `CLIPSYNC_RELAY_TOKEN_SECRET_FILE` | Optional private file of exact HMAC key bytes; at least 32 bytes; cannot be combined with `CLIPSYNC_RELAY_TOKEN_SECRET` |
| `CLIPSYNC_RELAY_TOKEN_SECRET` | HMAC token key; at least 32 bytes when explicitly set. Unset, empty, or whitespace-only generates a fresh random 32-byte key. |
| `CLIPSYNC_RELAY_WEBHOOK_URL` | Optional metadata webhook |
| `CLIPSYNC_RELAY_WEBHOOK_SECRET` | HMAC key for webhook signatures |
| `CLIPSYNC_RELAY_METRICS_TOKEN` | Bearer token that enables `GET /metrics`; at least 32 visible ASCII characters. Unset, empty, or whitespace-only leaves `/metrics` disabled (`404`). |
| `CLIPSYNC_RELAY_METRICS_TOKEN_FILE` | Optional private file containing the metrics bearer token; surrounding whitespace such as a trailing newline is trimmed; cannot be combined with `CLIPSYNC_RELAY_METRICS_TOKEN` |

## Token keys and rotation

The relay generates its default token key from the operating system's secure
random generator. Docker Compose forwards an optional
`CLIPSYNC_RELAY_TOKEN_SECRET` from the shell or project `.env`; it has no shared
default key. Empty and whitespace-only values also select random generation.
An explicit nonblank value shorter than 32 bytes prevents startup. Valid values
are used as their UTF-8 bytes without trimming or hex decoding.

To supply a persistent key, generate it once and retain it in your private
deployment environment:

```bash
export CLIPSYNC_RELAY_TOKEN_SECRET="$(openssl rand -hex 32)"
docker compose up --build
```

For credential-file deployments, `CLIPSYNC_RELAY_TOKEN_SECRET_FILE` reads the
file's exact bytes, including any newline, without hex decoding or trimming.
Both secret settings being present is an error, even when the environment value
is empty. Unset `CLIPSYNC_RELAY_TOKEN_SECRET` when selecting a file. The checked-in
Compose example forwards an optional environment key, so remove that mapping
in your own credential-file Compose configuration. Mount the file read-only,
with the same private permissions and relay-user access as the TLS key.
To migrate an existing environment key, store its exact UTF-8 bytes; do not
hex-decode a previously hexadecimal string or generate a replacement unless
rotating the key intentionally.

Use an independently generated value for `CLIPSYNC_RELAY_WEBHOOK_SECRET` if
enabling webhooks. Avoid publishing either key or resolved Compose output that
contains them.

With no explicit key, each process start creates a new key and invalidates old
tokens. Retaining a key does **not** retain rooms: the relay stores rooms,
pairing sessions, and queued text only in memory. Every relay restart loses that
state, so clients must pair again, including after a key rotation. To rotate,
replace the deployment key with a newly generated value, restart the relay,
then leave the old room on both clients (`clipsync room leave --yes`) and run
`room create` / `room join CODE` again.

## Metrics

`GET /metrics` reports live WebSocket connection counts, pairing
success/failure/throttled counters, and the resource-limit gauges and counters
listed under [Resource limits](#resource-limits). It is **disabled by default**: the route is not
registered and returns `404`. `GET /healthz` stays public for liveness checks.

To enable scraping, configure a dedicated bearer token, independent of the
token key and webhook secret:

```bash
export CLIPSYNC_RELAY_METRICS_TOKEN="$(openssl rand -hex 32)"
```

The relay then serves `/metrics` only to requests carrying
`Authorization: Bearer <token>` and answers anything else with `401` and a
`WWW-Authenticate: Bearer` challenge. The token is compared in constant time.
Values shorter than 32 bytes or containing spaces or control characters prevent
startup. For credential-file deployments, set
`CLIPSYNC_RELAY_METRICS_TOKEN_FILE` instead; the file uses the same private
permission rules as the token key file.

The endpoint shares the client listener, so a public reverse proxy forwards it
as well. Keep the token private and, where possible, block `/metrics` at the
proxy for public hostnames and scrape the local origin instead. A Prometheus
scrape job can read the same token file:

```yaml
scrape_configs:
  - job_name: clipsync-relay
    authorization:
      credentials_file: /etc/prometheus/clipsync-relay-metrics.token
    static_configs:
      - targets: ["127.0.0.1:7600"]
```

## Client

```bash
export CLIPSYNC_RELAY_URL=https://relay.example.com
clipsync init
clipsync room create
```

## Official OCI release images

The official package is `ghcr.io/jeon1691/clipsync-relay`. Starting with the next
release containing the container workflow, each genuine `vX.Y.Z` GitHub release
gets an OCI image for Linux `amd64` and `arm64`. There is no rolling `latest` tag.
Version tags are never replaced. The `container` workflow builds and tests each
architecture on a native GitHub-hosted runner before its publication job logs
in with the ephemeral `GITHUB_TOKEN`. A manual workflow run builds and tests
without publishing. Release publication must finish before any package write, and the tagged source
commit must belong to the current default branch. A tag run waits up to 15
minutes for release publication; if it times out, rerun it after the release
finishes. Existing published image versions are never overwritten on a rerun.

The image uses a digest-pinned Rust builder and distroless Debian runtime, runs
as UID/GID `65532`, and needs neither a shell nor a writable root filesystem.
The Dockerfile frontend, BuildKit and workflow actions are pinned, too. Source
checkout never persists Git credentials. Deployment keys and tokens are runtime
environment settings; do not pass them as image build arguments.

After a version is published, verify the **index digest** from its successful
`container` workflow summary before deployment. Install a current GitHub CLI
with `gh attestation` support and authenticate if required by the registry:

```bash
version=vX.Y.Z # choose an actual published version
image=ghcr.io/jeon1691/clipsync-relay
docker buildx imagetools inspect "$image:$version"
digest=sha256:REPLACE_WITH_THE_PUBLISHED_INDEX_DIGEST
gh attestation verify "oci://$image@$digest" \
  --repo Jeon1691/clipsync \
  --signer-workflow Jeon1691/clipsync/.github/workflows/container.yml \
  --source-ref "refs/tags/$version" \
  --deny-self-hosted-runners
```

For an offline provenance bundle, download `relay-container-provenance` from
the same workflow run and add `--bundle relay-index.sigstore.json` to verification.
Also use `--source-digest EXPECTED_TAG_COMMIT` when pinning a separately verified
source revision. The signed GitHub/Sigstore attestation covers the multiarch
index digest, which commits to both architecture manifests. BuildKit's unsigned
Docker archive metadata is not presented as a signed attestation or an SBOM.

To upgrade the existing Proxmox Compose deployment, retain its private `.env`,
existing token secret, public hostname and Cloudflare Tunnel configuration.
Add the verified index reference to that private `.env`:

```dotenv
CLIPSYNC_RELAY_IMAGE=ghcr.io/jeon1691/clipsync-relay@sha256:VERIFIED_INDEX_DIGEST
```

Then run on the deployment host:

```bash
cd /opt/clipsync/deploy/proxmox
docker compose pull relay
docker compose up -d --no-build relay
docker compose ps
curl -fsS http://127.0.0.1:7600/healthz
```

The host architecture selects its matching manifest. Keep the previous verified
index reference for rollback. A restart loses in-memory rooms and requires
clients to pair again. To continue building locally, leave
`CLIPSYNC_RELAY_IMAGE` unset and use `docker compose up -d --build`.

The first package publication needs Actions enabled and repository permission
to create/write its GHCR package. The workflow grants `packages: write` only to
the publication job, with `id-token: write` and `attestations: write` for signed
provenance. No long-lived package token is required. An existing package must
grant this repository Actions access. If GHCR creates it private, the maintainer
must deliberately set package visibility to public for anonymous pulls; the
workflow does not change account or package visibility settings. A successful
real publishing workflow is necessary for Scorecard Packaging evidence; merely
adding this file or passing a manual rehearsal is not publication evidence.
