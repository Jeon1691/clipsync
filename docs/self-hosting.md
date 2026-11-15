# Self-hosted relay

Hosted origin used by this project: `https://clipsync.develicit.dev`
(Proxmox + Cloudflare Tunnel; see `deploy/proxmox/`).

## Production (Proxmox)

The live relay runs on the Proxmox host at `/opt/clipsync`:

- container `clipsync-relay` bound to `127.0.0.1:7600`
- Cloudflare Tunnel hostname `clipsync.develicit.dev` → that origin
- public URL `https://clipsync.develicit.dev`

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

The relay listens on `0.0.0.0:7600`. Put TLS in front with Caddy or nginx.

```nginx
location / {
    proxy_pass http://127.0.0.1:7600;
    proxy_http_version 1.1;
    proxy_set_header Upgrade $http_upgrade;
    proxy_set_header Connection "upgrade";
    proxy_read_timeout 3600;
}
```

## Environment

| Variable | Meaning |
| --- | --- |
| `CLIPSYNC_RELAY_BIND` | Listen address, default `0.0.0.0:7600` |
| `CLIPSYNC_RELAY_PUBLIC_URL` | URL clients should use |
| `CLIPSYNC_RELAY_TOKEN_SECRET` | HMAC token key; at least 32 bytes when explicitly set. Unset, empty, or whitespace-only generates a fresh random 32-byte key. |
| `CLIPSYNC_RELAY_WEBHOOK_URL` | Optional metadata webhook |
| `CLIPSYNC_RELAY_WEBHOOK_SECRET` | HMAC key for webhook signatures |

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

## Client

```bash
export CLIPSYNC_RELAY_URL=https://relay.example.com
clipsync init
clipsync room create
```
