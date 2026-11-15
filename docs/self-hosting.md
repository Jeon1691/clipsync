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
