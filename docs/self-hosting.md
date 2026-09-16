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
| `CLIPSYNC_RELAY_TOKEN_SECRET` | HMAC secret for device tokens |
| `CLIPSYNC_RELAY_WEBHOOK_URL` | Optional metadata webhook |
| `CLIPSYNC_RELAY_WEBHOOK_SECRET` | HMAC key for webhook signatures |

## Client

```bash
export CLIPSYNC_RELAY_URL=https://relay.example.com
clipsync init
clipsync room create
```
