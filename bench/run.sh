#!/usr/bin/env bash
# Issue #13: end-to-end 50k-doc benchmark, Wayfinder vs Solr 9.
#
# Generates a deterministic 50k corpus, indexes it into both a native
# release Wayfinder binary and a real Solr 9 (Docker), measures resident
# memory (startup idle, post-index before query load, and under a
# facet+filter+highlight query load), cold start to
# first query, p95 query latency (warm cache: the same query N_QUERIES
# times; and cold cache: one query per distinct corpus term after a core
# RELOAD flushes Solr's caches -- issue #251), container image size, and
# index size on disk -- then renders `docs/benchmarks.md` via
# `wayfinder_bench::results`.
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
# Written by gen_corpus from wayfinder_bench::corpus::query_terms(): the
# corpus's distinct query vocabulary, one term per line. The cold pass
# queries these rather than a second word list hardcoded here (issue #251).
TERMS_FILE="$CORPUS_DIR/terms.txt"
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

query_result_cache_stat() { # base_url core stat_name -> the named CACHE.searcher.queryResultCache counter
  # `base` here is the SERVER root (e.g. http://host:port/solr), not a core
  # URL: admin/metrics is a server-level handler and reports every core as a
  # `solr.core.<core>` registry, so the core is a parse-time selector rather
  # than part of the path. The parse lives in bench/query_result_cache_stat.py
  # (pinned by bench/tests/query_result_cache_metrics.rs against a real Solr 9
  # capture) instead of an inline heredoc, so it is testable in isolation.
  #
  # The previous admin/mbeans path never worked against a real Solr 9 (issue
  # #251): without `json.nl=map` the `solr-mbeans` body is a type signature,
  # not JSON, and the stats key is `.searcher`-scoped. Do not go back to it.
  #
  # `indent=true` is load-bearing, not cosmetic. Verified live against solr:9:
  # `admin/metrics?...&wt=json` with no response-writer param renders the same
  # type-signature body the mbeans path did -- HTTP 200, unquoted keys, values
  # literally `int`/`float`, no responseHeader, not parseable as JSON. Passing
  # any recognized writer param (`indent=true`, `indent=false`, `json.nl=map`)
  # switches it to real JSON; an unrecognized one (`x=1`) does not.
  local base=$1 core=$2 stat=$3
  curl -sSf "$base/admin/metrics?group=core&prefix=CACHE.searcher.queryResultCache&wt=json&indent=true" \
    | python3 "$HERE/query_result_cache_stat.py" "$core" "$stat"
}

query_result_cache_hits() { # base_url core -> cumulative queryResultCache hit count
  query_result_cache_stat "$1" "$2" hits
}

query_result_cache_lookups() { # base_url core -> cumulative queryResultCache lookup count
  query_result_cache_stat "$1" "$2" lookups
}

warm_up_pass() { # base_url core terms_file -> GETs every term once, results discarded
  # Equalises OS page cache and JVM JIT state across both engines before
  # anything is timed, so the cold/warm difference measured afterwards is
  # cache state and not first-touch cost.
  local base=$1 core=$2 terms_file=$3
  local term result status
  while read -r term; do
    [ -n "$term" ] || continue
    result=$(curl -s -o /dev/null -w '%{http_code}' \
      "$base/$core/select?q=$term&defType=edismax&qf=title+body&fq=category:animals&facet=true&facet.field=category&hl=true&hl.fl=body&rows=10&wt=json")
    status="$result"
    if [ "${status:0:1}" != "2" ]; then
      echo "warm_up_pass: non-2xx response ($status) for term '$term' from terms.txt ($terms_file)" >&2
      return 1
    fi
  done < "$terms_file"
}

flush_solr_caches() { # -> reopens the searcher, zeroing the query caches
  # A core RELOAD is the flush. `update?commit=true` does NOT work here:
  # Solr skips the commit when nothing has changed, so no new searcher opens
  # and the caches survive. That cost a full round of bad measurements
  # (issue #251); bench/tests/run_sh_cold_warm.rs guards against it coming
  # back. The caller pings afterwards -- the core is briefly unavailable
  # while it reloads.
  curl -sSf "$SOLR_URL/admin/cores?action=RELOAD&core=$SOLR_CORE&wt=json" >/dev/null
}

run_cold_query_pass() { # base_url core terms_file out_latency_file -> one query per term, latencies to the file
  # Same query shape as run_query_load, with `q` varying over every distinct
  # corpus term, so no two requests in this pass can share a cache entry.
  local base=$1 core=$2 terms_file=$3 outfile=$4
  local term result status
  : > "$outfile"
  while read -r term; do
    [ -n "$term" ] || continue
    result=$(curl -s -o /dev/null -w '%{http_code} %{time_total}' \
      "$base/$core/select?q=$term&defType=edismax&qf=title+body&fq=category:animals&facet=true&facet.field=category&hl=true&hl.fl=body&rows=10&wt=json")
    status="${result%% *}"
    if [ "${status:0:1}" != "2" ]; then
      echo "run_cold_query_pass: non-2xx response ($status) for term '$term' from terms.txt ($terms_file)" >&2
      return 1
    fi
    echo "${result#* }" | awk '{printf "%.2f\n", $1*1000}' >> "$outfile"
  done < "$terms_file"
}

assert_cache_pass_behavior() { # kind hits lookups n_queries -- hits/lookups are deltas from query_result_cache_hits/query_result_cache_lookups across the pass
  # A cold/warm split that silently degenerates into measuring the same
  # thing twice is worse than no split, because it looks like evidence.
  local kind=$1 hits=$2 lookups=$3 n_queries=$4
  local min_hits
  case "$kind" in
    cold)
      # 0 lookups is not evidence of a cold cache -- it is evidence nothing
      # was measured: no query reached Solr's searcher at all, and hits is
      # then trivially 0. Accepting that as a clean cold pass would let a
      # silently-empty run report itself as the headline p95 (issue #251).
      if [ "$lookups" -eq 0 ]; then
        echo "assert_cache_pass_behavior: cold pass took 0 queryResultCache lookups, so no query reached Solr's searcher -- these are not cold numbers, they are no numbers" >&2
        return 1
      fi
      if [ "$hits" -ne 0 ]; then
        echo "assert_cache_pass_behavior: cold pass took $hits cache hits over $lookups lookups, expected 0 -- the searcher was not actually flushed, or the term list repeated a query, so these are not cold numbers" >&2
        return 1
      fi
      ;;
    warm)
      min_hits=$(( n_queries - 2 ))
      if [ "$hits" -lt "$min_hits" ]; then
        echo "assert_cache_pass_behavior: warm pass took $hits cache hits over $lookups lookups, expected at least $min_hits of $n_queries queries -- these are not warm numbers" >&2
        return 1
      fi
      ;;
    *)
      echo "assert_cache_pass_behavior: unknown pass kind '$kind', expected cold or warm" >&2
      return 1
      ;;
  esac
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

echo "== wayfinder warm-up pass (results discarded) =="
warm_up_pass "http://$WF_BIND/solr" "$SOLR_CORE" "$TERMS_FILE"

# Wayfinder has no query result cache, so there is nothing to flush between
# the two passes and no cache counters to assert on: its cold and warm
# numbers differ only by whatever the OS page cache does. The Solr section
# below flushes and asserts, which is where the split is load-bearing.
WF_LATENCIES_COLD="$WORK/wf_latencies_cold.txt"
echo "== wayfinder cold pass (one query per distinct term) =="
run_cold_query_pass "http://$WF_BIND/solr" "$SOLR_CORE" "$TERMS_FILE" "$WF_LATENCIES_COLD"

WF_LATENCIES_WARM="$WORK/wf_latencies_warm.txt"
run_query_load "http://$WF_BIND/solr" "$SOLR_CORE" "$WF_LATENCIES_WARM" &
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

# Smoke-probe the cache counter read before anything expensive happens. The
# first real read used to sit after the whole Wayfinder phase and Solr
# indexing, so a broken read (which is exactly what shipped: see
# query_result_cache_stat) aborted the run roughly an hour in. Fail here
# instead, seconds after the core answers its first ping.
if ! SOLR_CACHE_PROBE=$(query_result_cache_hits "$SOLR_URL" "$SOLR_CORE"); then
  echo "queryResultCache counter smoke probe failed against $SOLR_URL (core $SOLR_CORE) -- aborting before the expensive phases rather than an hour in" >&2
  exit 1
fi
echo "solr queryResultCache counter probe ok (hits=$SOLR_CACHE_PROBE)"

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
SOLR_STARTUP_IDLE_MB=$(solr_mem_mb)
echo "solr startup idle mem: ${SOLR_STARTUP_IDLE_MB}MB"

echo "== indexing solr =="
index_corpus "$SOLR_URL" "$SOLR_CORE"

sleep 1
SOLR_POST_INDEX_MB=$(solr_mem_mb)
echo "solr post-index mem: ${SOLR_POST_INDEX_MB}MB"

echo "== solr warm-up pass (results discarded) =="
warm_up_pass "$SOLR_URL" "$SOLR_CORE" "$TERMS_FILE"

echo "== flushing solr caches (core RELOAD) =="
flush_solr_caches
# The core is briefly unavailable while it reloads; wait for it to serve again.
wait_for_ping "$SOLR_URL/$SOLR_CORE/admin/ping?wt=json" >/dev/null

SOLR_LATENCIES_COLD="$WORK/solr_latencies_cold.txt"
SOLR_COLD_HITS_BEFORE=$(query_result_cache_hits "$SOLR_URL" "$SOLR_CORE")
SOLR_COLD_LOOKUPS_BEFORE=$(query_result_cache_lookups "$SOLR_URL" "$SOLR_CORE")
echo "== solr cold pass (one query per distinct term) =="
run_cold_query_pass "$SOLR_URL" "$SOLR_CORE" "$TERMS_FILE" "$SOLR_LATENCIES_COLD"
SOLR_COLD_HITS_AFTER=$(query_result_cache_hits "$SOLR_URL" "$SOLR_CORE")
SOLR_COLD_LOOKUPS_AFTER=$(query_result_cache_lookups "$SOLR_URL" "$SOLR_CORE")
assert_cache_pass_behavior cold \
  $(( SOLR_COLD_HITS_AFTER - SOLR_COLD_HITS_BEFORE )) \
  $(( SOLR_COLD_LOOKUPS_AFTER - SOLR_COLD_LOOKUPS_BEFORE )) \
  "$N_QUERIES"

SOLR_LATENCIES_WARM="$WORK/solr_latencies_warm.txt"
SOLR_WARM_HITS_BEFORE=$(query_result_cache_hits "$SOLR_URL" "$SOLR_CORE")
SOLR_WARM_LOOKUPS_BEFORE=$(query_result_cache_lookups "$SOLR_URL" "$SOLR_CORE")
run_query_load "$SOLR_URL" "$SOLR_CORE" "$SOLR_LATENCIES_WARM" &
LOAD_PID=$!
SOLR_LOAD_MB=0
while kill -0 "$LOAD_PID" 2>/dev/null; do
  sample=$(solr_mem_mb)
  if awk -v a="$sample" -v b="$SOLR_LOAD_MB" 'BEGIN{exit !(a>b)}'; then SOLR_LOAD_MB=$sample; fi
  sleep 0.2
done
wait "$LOAD_PID"
echo "solr load mem (max sampled): ${SOLR_LOAD_MB}MB"
SOLR_WARM_HITS_AFTER=$(query_result_cache_hits "$SOLR_URL" "$SOLR_CORE")
SOLR_WARM_LOOKUPS_AFTER=$(query_result_cache_lookups "$SOLR_URL" "$SOLR_CORE")
assert_cache_pass_behavior warm \
  $(( SOLR_WARM_HITS_AFTER - SOLR_WARM_HITS_BEFORE )) \
  $(( SOLR_WARM_LOOKUPS_AFTER - SOLR_WARM_LOOKUPS_BEFORE )) \
  "$N_QUERIES"

SOLR_INDEX_KB=$(docker exec "$SOLR_CONTAINER" du -sk "/var/solr/data/$SOLR_CORE/data/index" | awk '{print $1}')
SOLR_INDEX_MB=$(awk -v k="$SOLR_INDEX_KB" 'BEGIN { printf "%.2f", k / 1024 }')

SOLR_IMAGE_BYTES="$(docker inspect --format='{{.Size}}' solr:9)"
SOLR_IMAGE_MB=$(awk -v b="$SOLR_IMAGE_BYTES" 'BEGIN { printf "%.2f", b / 1048576 }')

docker rm -f "$SOLR_CONTAINER" >/dev/null 2>&1 || true

# --- Render ----------------------------------------------------------------
echo "== rendering docs/benchmarks.md =="
"$BENCH_BIN/render_report" \
  "$SOLR_STARTUP_IDLE_MB" "$SOLR_POST_INDEX_MB" "$SOLR_LOAD_MB" "$SOLR_COLD_MS" "$SOLR_IMAGE_MB" "$SOLR_INDEX_MB" "$SOLR_LATENCIES_WARM" "$SOLR_LATENCIES_COLD" \
  "$WF_STARTUP_IDLE_MB" "$WF_POST_INDEX_MB" "$WF_LOAD_MB" "$WF_COLD_MS" "$WF_IMAGE_MB" "$WF_INDEX_MB" "$WF_LATENCIES_WARM" "$WF_LATENCIES_COLD" \
  "$SIZE" \
  "$ROOT/docs/benchmarks.md"

echo "done: see $ROOT/docs/benchmarks.md"
