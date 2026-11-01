#!/usr/bin/env bash
# Start a throwaway Home Assistant on http://127.0.0.1:8123 for the live test
# and manual testing. State lives in a temporary copy of testing/ha-config.
#   testing/run-ha.sh          start (prints the state directory)
#   testing/run-ha.sh stop     stop and remove the container
set -euo pipefail
NAME=hassdash4linux-test-ha
IMAGE=${HA_IMAGE:-ghcr.io/home-assistant/home-assistant:stable}
if [[ "${1:-}" == "stop" ]]; then
  docker rm -f "$NAME" >/dev/null 2>&1 || true
  exit 0
fi
STATE=$(mktemp -d -t hd-test-ha.XXXXXX)
cp "$(dirname "$0")"/ha-config/*.yaml "$STATE"/
docker rm -f "$NAME" >/dev/null 2>&1 || true
docker run -d --name "$NAME" -p 127.0.0.1:8123:8123 -v "$STATE":/config:Z "$IMAGE" >/dev/null
echo "state: $STATE"
echo -n "waiting for Home Assistant"
for _ in $(seq 1 120); do
  if curl -fsS http://127.0.0.1:8123/auth/providers >/dev/null 2>&1 || curl -fsS http://127.0.0.1:8123/api/onboarding >/dev/null 2>&1; then
    echo " ready"; exit 0
  fi
  echo -n .; sleep 2
done
echo " timed out"; docker logs --tail 50 "$NAME"; exit 1
