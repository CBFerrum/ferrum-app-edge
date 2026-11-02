#!/usr/bin/env bash
# Does a release start where it has to? Each image is a Linux a gateway might be running, and the binary
# takes nothing from the machine that built it. It runs, answers on its console, gets an answer over
# HTTPS by name, and stops as it would under systemd.
#
#     scripts/starts.sh out/ferrum-site-x86_64-linux ubuntu:22.04 centos:7
set -euo pipefail

binary=$1
shift
config=$(mktemp)
trap 'rm -f "$config"' EXIT
# Any HTTPS host will do: being turned away means the name resolved and TLS held.
cat > "$config" <<'YAML'
server: https://github.com
tenant: t
state_dir: /tmp/site
console: "0.0.0.0:7071"
peer_only: true
adapters:
  - {name: press, kind: sim, devices: [PRESS-1]}
YAML

for image in "$@"; do
  id=$(docker create -p 127.0.0.1:7071:7071 "$image" /ferrum-site run --config /site.yaml)
  docker cp -q "$binary" "$id:/ferrum-site"
  docker cp -q "$config" "$id:/site.yaml"
  docker start "$id" > /dev/null
  up=
  for _ in $(seq 30); do
    if curl -sf http://127.0.0.1:7071/metrics > /dev/null; then up=1; break; fi
    sleep 1
  done
  said=$(docker exec "$id" /ferrum-site enroll --config /site.yaml --code 0 2>&1 || true)
  docker stop -t 10 "$id" > /dev/null
  code=$(docker inspect -f '{{.State.ExitCode}}' "$id")
  logs=$(docker logs "$id" 2>&1)
  docker rm "$id" > /dev/null
  if [[ -z $up || $code != 0 ]] || ! grep -qE '^ferrum-site: [0-9]{3} ' <<< "$said"; then
    echo "$image: up=${up:-no} exit=$code enroll said: $said"
    echo "$logs"
    exit 1
  fi
  echo "$image: started, answered, reached https, stopped"
done
