#!/usr/bin/env bash
# Copyright the ClipSync contributors.
# SPDX-License-Identifier: MIT
set -euo pipefail
image="${1:?Usage: smoke-relay-container.sh IMAGE}"
[[ "$(docker image inspect "$image" --format '{{.Config.User}}')" == '65532:65532' ]]
docker run --rm --network none --read-only --cap-drop ALL \
  --security-opt no-new-privileges "$image" --help >/dev/null
health_file="$(mktemp)"
container="$(docker run --detach --read-only --cap-drop ALL \
  --security-opt no-new-privileges --publish 127.0.0.1::7600 "$image")"
trap 'docker rm --force "$container" >/dev/null; rm -f "$health_file"' EXIT
address="$(docker port "$container" 7600/tcp)"
for attempt in {1..60}; do
  if curl --fail --silent --noproxy '*' "http://${address}/healthz" > "$health_file"; then
    python3 -c 'import json, sys; assert json.load(sys.stdin) == {"ok": True, "service": "clipsync-relay"}' \
      < "$health_file"
    exit 0
  fi
  if [[ "$(docker inspect "$container" --format '{{.State.Running}}')" != true ]]; then
    docker logs "$container"
    exit 1
  fi
  sleep 1
done
docker logs "$container"
echo 'Relay container did not become healthy.' >&2
exit 1
