#!/usr/bin/env bash
# Live Docker gate: deliberately separate from hermetic cargo tests.
set -euo pipefail

image="${WAYFINDER_SMOKE_IMAGE:-wayfinder-extraction-smoke:local}"
curl_timeout=(--connect-timeout 2 --max-time 10)
workdir="$(mktemp -d)"
container=""
cleanup() {
  if [[ -n "$container" ]]; then docker rm -f "$container" >/dev/null 2>&1 || true; fi
  rm -rf "$workdir"
}
trap cleanup EXIT

cat >"$workdir/schema.toml" <<'EOF'
[core]
name = "smoke"
unique_key = "id"
default_field = "body"

[[fields]]
name = "id"
type = "string"
stored = true
required = true

[[fields]]
name = "body"
type = "text_general"
stored = true
EOF
printf 'container extraction smoke\n' >"$workdir/upload.txt"
mkdir "$workdir/data"
# The container intentionally runs without root; this temporary bind mount must
# be writable by its fixed runtime UID/GID.
chmod 0777 "$workdir/data"

docker build --tag "$image" .
container="$(docker run --detach --user 65532:65532 --publish 127.0.0.1::8983 \
  --volume "$workdir/schema.toml:/schema.toml:ro" \
  --volume "$workdir/data:/data" \
  "$image" /schema.toml /data 0.0.0.0:8983)"
port="$(docker port "$container" 8983/tcp | sed -n 's/.*:\([0-9][0-9]*\)$/\1/p')"
[[ -n "$port" ]] || { echo "could not determine smoke port" >&2; exit 1; }
base="http://127.0.0.1:$port"

for _ in $(seq 1 100); do
  if curl "${curl_timeout[@]}" --fail --silent --show-error \
    "$base/wayfinder/smoke/admin/ping?wt=json" >/dev/null; then break; fi
  sleep 0.1
done
curl "${curl_timeout[@]}" --fail --silent --show-error \
  "$base/wayfinder/smoke/admin/ping?wt=json" >/dev/null
response="$(curl "${curl_timeout[@]}" --fail --silent --show-error --request POST \
  --form "file=@$workdir/upload.txt;type=text/plain" \
  "$base/wayfinder/smoke/update/extract?extractOnly=true&wt=json")"
printf '%s\n' "$response" | grep -F 'container extraction smoke' >/dev/null
