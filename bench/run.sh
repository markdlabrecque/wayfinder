#!/usr/bin/env bash
# Issue #13: end-to-end 50k-doc benchmark, Wayfinder vs Solr 9.
#
# Generates a deterministic 50k corpus, indexes it into both a native
# release Wayfinder binary and a real Solr 9 (Docker), measures resident
# memory (startup idle, post-index before query load, and under a
# facet+filter+highlight query load), cold start to
# first query, p95 query latency, container image size, and index size on
# disk -- then renders `docs/benchmarks.md` via `wayfinder_bench::results`.
#
# Requires: docker, curl, cargo. Mirrors solr-ref/capture.sh's Docker
# conventions. Not run in CI; this is a manual/local benchmark tool.
#
# Usage: bench/run.sh [seed] [size]
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/.." && pwd)"

SEED="${1:-42}"
SIZE="${2:-50000}"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

CORPUS_DIR="$WORK/corpus"
SCHEMA_TOML="$CORPUS_DIR/schema.toml"
WF_DATA="$WORK/wf-data"
mkdir -p "$WF_DATA"

WF_BIND="127.0.0.1:18984"
# Host port for Solr is overridable: this repo's other worktrees each run
# their own capture.sh Solr container on their own port (8983-8989, 8994 in
# use at the time this was written), and the shared Docker daemon means a
# hardcoded 8983 collides with them.
SOLR_HOST_PORT="${SOLR_HOST_PORT:-18983}"
SOLR_URL="http://localhost:$SOLR_HOST_PORT/solr"
SOLR_CONTAINER="wayfinder-bench-solr-13"
SOLR_CORE="content"
WF_IMAGE_TAG="wayfinder-bench-image:bench"
N_QUERIES="${N_QUERIES:-200}"

echo "== building bench tooling =="
cargo build --release --manifest-path "$HERE/Cargo.toml"
BENCH_BIN="$HERE/target/release"

echo "== generating corpus: seed=$SEED size=$SIZE =="
"$BENCH_BIN/gen_corpus" "$SEED" "$SIZE" "$CORPUS_DIR"

# POST each batch file with commit=false, then a final explicit commit --
# realistic bulk-load shape, and keeps every request under Wayfinder's
# default 2 MB body-size limit (see gen_corpus.rs doc comment).
index_corpus() { # base_url core
  local base=$1 core=$2
  for batch in "$CORPUS_DIR"/batch-*.json; do
    curl -sSf "$base/$core/update?commit=false" \
      -H 'Content-Type: application/json' \
      --data-binary "@$batch" >/dev/null
  done
  curl -sSf "$base/$core/update?commit=true" \
    -H 'Content-Type: application/json' -d '[]' >/dev/null
}

echo "== building wayfinder release binary (native, for the memory/latency run) =="
cargo build --release --manifest-path "$ROOT/Cargo.toml"
WF_BIN="$ROOT/target/release/wayfinder"

echo "== building wayfinder container image (for the image-size metric) =="
docker build -q -f "$ROOT/Dockerfile" -t "$WF_IMAGE_TAG" "$ROOT" >/dev/null
WF_IMAGE_BYTES="$(docker inspect --format='{{.Size}}' "$WF_IMAGE_TAG")"
WF_IMAGE_MB=$(awk -v b="$WF_IMAGE_BYTES" 'BEGIN { printf "%.2f", b / 1048576 }')

pids_rss_kb() { # sum RSS (KB) of a process and all its children, best-effort
  local pid=$1
  ps -o rss= -p "$pid" 2>/dev/null | awk '{s+=$1} END {print s+0}'
}

solr_mem_mb() {
  # docker's MemUsage field has no space between the number and its unit
  # (e.g. "2.10GiB", not "2.10 GiB"), so splitting on whitespace never
  # isolates the unit -- match it against the raw token instead. Units are
  # matched longest/most-specific-suffix first (TiB/GiB/MiB/KiB before the
  # bare "B") so e.g. "KiB" doesn't fall through to the plain-bytes branch.
  # An unrecognized unit fails loudly rather than silently defaulting to
  # MiB scaling (issue #62).
  docker stats --no-stream --format '{{.MemUsage}}' "$SOLR_CONTAINER" \
    | awk -F'/' '{print $1}' \
    | awk '{
        s=$1;
        if (s ~ /^[0-9.]+TiB$/) { gsub(/TiB/, "", s); v = s * 1024 * 1024; }
        else if (s ~ /^[0-9.]+GiB$/) { gsub(/GiB/, "", s); v = s * 1024; }
        else if (s ~ /^[0-9.]+MiB$/) { gsub(/MiB/, "", s); v = s; }
        else if (s ~ /^[0-9.]+KiB$/) { gsub(/KiB/, "", s); v = s / 1024; }
        else if (s ~ /^[0-9.]+B$/) { gsub(/B/, "", s); v = s / 1048576; }
        else {
          print "solr_mem_mb: unrecognized memory unit in \x27" s "\x27" > "/dev/stderr";
          exit 1;
        }
        printf "%.2f", v
      }'
}

wait_for_ping() { # url -> echoes cold-start ms on stdout
  local url=$1
  local start_ns
  start_ns=$(date +%s%N)
  for _ in $(seq 300); do
    if curl -sf "$url" >/dev/null 2>&1; then
      local end_ns
      end_ns=$(date +%s%N)
      echo $(( (end_ns - start_ns) / 1000000 ))
      return 0
    fi
    sleep 0.1
  done
  echo "timed out waiting for $url" >&2
  return 1
}

check_schema_add_field_response() { # status body -> non-zero + body on stderr unless a clean 2xx
  local status=$1 body=$2
  if [[ "$status" != 2* ]]; then
    echo "$body" >&2
    return 1
  fi
  # Solr's schema API can return HTTP 200 with an "errors" body on a
  # rejected add-field, so a 2xx status alone isn't sufficient.
  if echo "$body" | grep -q '"errors"'; then
    echo "$body" >&2
    return 1
  fi
  return 0
}

run_query_load() { # base_url core out_latency_file -> prints max RSS sample seen (mem_sampler_pid mem_out)
  local base=$1 core=$2 outfile=$3
  : > "$outfile"
  for _ in $(seq 1 "$N_QUERIES"); do
    local result status
    result=$(curl -s -o /dev/null -w '%{http_code} %{time_total}' \
      "$base/$core/select?q=rocket&defType=edismax&qf=title+body&fq=category:animals&facet=true&facet.field=category&hl=true&hl.fl=body&rows=10&wt=json")
    status="${result%% *}"
    if [ "${status:0:1}" != "2" ]; then
      echo "run_query_load: non-2xx response ($status) from $base/$core/select" >&2
      return 1
    fi
    echo "${result#* }" | awk '{printf "%.2f\n", $1*1000}' >> "$outfile"
  done
}

# --- Wayfinder ---------------------------------------------------------
echo "== starting wayfinder =="
"$WF_BIN" "$SCHEMA_TOML" "$WF_DATA" "$WF_BIND" > "$WORK/wf.log" 2>&1 &
WF_PID=$!
WF_COLD_MS=$(wait_for_ping "http://$WF_BIND/solr/content/admin/ping?wt=json")
echo "wayfinder cold start: ${WF_COLD_MS}ms"
WF_STARTUP_IDLE_KB=$(pids_rss_kb "$WF_PID")
WF_STARTUP_IDLE_MB=$(awk -v k="$WF_STARTUP_IDLE_KB" 'BEGIN { printf "%.2f", k / 1024 }')
echo "wayfinder startup idle mem: ${WF_STARTUP_IDLE_MB}MB"

echo "== indexing wayfinder =="
index_corpus "http://$WF_BIND/solr" "$SOLR_CORE"

sleep 1
WF_POST_INDEX_KB=$(pids_rss_kb "$WF_PID")
WF_POST_INDEX_MB=$(awk -v k="$WF_POST_INDEX_KB" 'BEGIN { printf "%.2f", k / 1024 }')
echo "wayfinder post-index mem: ${WF_POST_INDEX_MB}MB"

WF_LATENCIES="$WORK/wf_latencies.txt"
run_query_load "http://$WF_BIND/solr" "$SOLR_CORE" "$WF_LATENCIES" &
LOAD_PID=$!
WF_LOAD_KB=0
while kill -0 "$LOAD_PID" 2>/dev/null; do
  sample=$(pids_rss_kb "$WF_PID")
  if [ "$sample" -gt "$WF_LOAD_KB" ]; then WF_LOAD_KB=$sample; fi
  sleep 0.1
done
wait "$LOAD_PID"
WF_LOAD_MB=$(awk -v k="$WF_LOAD_KB" 'BEGIN { printf "%.2f", k / 1024 }')
echo "wayfinder load mem (max RSS during load): ${WF_LOAD_MB}MB"

WF_INDEX_KB=$(du -sk "$WF_DATA" | awk '{print $1}')
WF_INDEX_MB=$(awk -v k="$WF_INDEX_KB" 'BEGIN { printf "%.2f", k / 1024 }')

kill "$WF_PID" 2>/dev/null || true
wait "$WF_PID" 2>/dev/null || true

# --- Solr ----------------------------------------------------------------
echo "== starting solr =="
docker rm -f "$SOLR_CONTAINER" >/dev/null 2>&1 || true
# NOTE: SOLR_COLD_MS (via wait_for_ping below) starts timing only after this
# `docker run -d` returns, so container-create time is excluded from the
# reported cold-start number -- see issue #62.
docker run -d --name "$SOLR_CONTAINER" -p "$SOLR_HOST_PORT:8983" solr:9 solr-precreate "$SOLR_CORE" >/dev/null
SOLR_COLD_MS=$(wait_for_ping "$SOLR_URL/$SOLR_CORE/admin/ping?wt=json")
echo "solr cold start: ${SOLR_COLD_MS}ms"
SOLR_STARTUP_IDLE_MB=$(solr_mem_mb)
echo "solr startup idle mem: ${SOLR_STARTUP_IDLE_MB}MB"

SCHEMA_BODY_FILE="$WORK/schema_resp.json"
SCHEMA_STATUS=$(curl -sS -o "$SCHEMA_BODY_FILE" -w '%{http_code}' \
  "$SOLR_URL/$SOLR_CORE/schema" -H 'Content-Type: application/json' -d '{
  "add-field": [
    {"name":"title",    "type":"text_en", "indexed":true, "stored":true},
    {"name":"body",     "type":"text_en", "indexed":true, "stored":true},
    {"name":"category", "type":"string",  "indexed":true, "stored":true,
     "docValues":true, "multiValued":true}
  ]
}')
SCHEMA_BODY="$(cat "$SCHEMA_BODY_FILE")"
if ! check_schema_add_field_response "$SCHEMA_STATUS" "$SCHEMA_BODY"; then
  echo "solr schema add-field failed (status $SCHEMA_STATUS)" >&2
  exit 1
fi

echo "== indexing solr =="
index_corpus "$SOLR_URL" "$SOLR_CORE"

sleep 1
SOLR_POST_INDEX_MB=$(solr_mem_mb)
echo "solr post-index mem: ${SOLR_POST_INDEX_MB}MB"

SOLR_LATENCIES="$WORK/solr_latencies.txt"
run_query_load "$SOLR_URL" "$SOLR_CORE" "$SOLR_LATENCIES" &
LOAD_PID=$!
SOLR_LOAD_MB=0
while kill -0 "$LOAD_PID" 2>/dev/null; do
  sample=$(solr_mem_mb)
  if awk -v a="$sample" -v b="$SOLR_LOAD_MB" 'BEGIN{exit !(a>b)}'; then SOLR_LOAD_MB=$sample; fi
  sleep 0.2
done
wait "$LOAD_PID"
echo "solr load mem (max sampled): ${SOLR_LOAD_MB}MB"

SOLR_INDEX_KB=$(docker exec "$SOLR_CONTAINER" du -sk "/var/solr/data/$SOLR_CORE/data" | awk '{print $1}')
SOLR_INDEX_MB=$(awk -v k="$SOLR_INDEX_KB" 'BEGIN { printf "%.2f", k / 1024 }')

SOLR_IMAGE_BYTES="$(docker inspect --format='{{.Size}}' solr:9)"
SOLR_IMAGE_MB=$(awk -v b="$SOLR_IMAGE_BYTES" 'BEGIN { printf "%.2f", b / 1048576 }')

docker rm -f "$SOLR_CONTAINER" >/dev/null 2>&1 || true

# --- Render ----------------------------------------------------------------
echo "== rendering docs/benchmarks.md =="
"$BENCH_BIN/render_report" \
  "$SOLR_STARTUP_IDLE_MB" "$SOLR_POST_INDEX_MB" "$SOLR_LOAD_MB" "$SOLR_COLD_MS" "$SOLR_IMAGE_MB" "$SOLR_INDEX_MB" "$SOLR_LATENCIES" \
  "$WF_STARTUP_IDLE_MB" "$WF_POST_INDEX_MB" "$WF_LOAD_MB" "$WF_COLD_MS" "$WF_IMAGE_MB" "$WF_INDEX_MB" "$WF_LATENCIES" \
  "$SIZE" \
  "$ROOT/docs/benchmarks.md"

echo "done: see $ROOT/docs/benchmarks.md"
