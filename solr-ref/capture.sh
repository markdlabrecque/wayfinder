#!/usr/bin/env bash
# Capture reference /select responses from a real Solr for the tracer-bullet schema.
# Output: solr-ref/responses/*.json + manifest.tsv (query -> file)
#
# Usage:
#   capture.sh                  full re-capture: wipes responses/ and every manifest
#   capture.sh --only <regex>   capture only fixtures whose name matches <regex>,
#                               leaving every other fixture and manifest row untouched
#
# Env: SOLR_PORT (default 8983) -- the host port for the main `content` core's
#      container, for when something else local already holds 8983.
#      KEEP_CONTAINERS=1 -- leave every block's container running afterwards
#      instead of releasing it (see `release` below; the default exists because
#      twenty concurrent solr:9 containers exhaust Docker's memory).
#
# `--only` exists because a full run rewrites all 400+ fixtures, and the QTime /
# _version_ / rid churn that produces dirties every concurrent branch's diff (see
# CLAUDE.md, "Never re-capture existing fixtures as a side effect"). A new capture
# block should therefore be added at the end of this script and run with
# `--only '^myprefix_'`, which is also what makes two branches able to capture
# without clobbering each other's uncommitted fixtures.
#
# A block that starts its own container can skip that setup when nothing in the
# block is wanted by guarding it with `want_any '^myprefix_'`. A filtered run
# still walks the whole script, so it still starts the containers of blocks that
# carry no such guard -- it takes minutes, and its guarantee is about what it
# writes, not about how fast it is. It does not accumulate containers while
# doing so: each block releases its own (see `release`).
set -euo pipefail

ONLY=""
while [ $# -gt 0 ]; do
  case "$1" in
    --only)
      [ $# -ge 2 ] || { echo "capture.sh: --only needs a regex" >&2; exit 2; }
      ONLY=$2; shift 2 ;;
    --only=*) ONLY=${1#--only=}; shift ;;
    -h|--help) sed -n '2,24p' "${BASH_SOURCE[0]}"; exit 0 ;;
    *) echo "capture.sh: unknown argument '$1'" >&2; exit 2 ;;
  esac
done

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OUT="$HERE/responses"
CORE=content
# Overridable because 8983 is the port every other local Solr wants too -- a DDEV
# router or a stray `docker-solr` holding it makes the `docker run` below fail
# under `set -e` before anything is captured. Nothing port-dependent reaches the
# committed data: `manifest.tsv` rows are core-relative and carry no host.
SOLR_PORT=${SOLR_PORT:-8983}
SOLR=http://localhost:$SOLR_PORT/solr
CONTAINER=wayfinder-solr-ref

# --- `--only` filtering ----------------------------------------------------
# Every capture helper below starts with `want "$name" || return 0`, so a
# filtered run performs no request and writes no file for a name that does not
# match. `want_any` is the same predicate for a whole block, for guarding the
# container setup a block needs before it can capture anything.
want() { [ -z "$ONLY" ] || [[ $1 =~ $ONLY ]]; }
# Heuristic, and deliberately generous: a block guard sees only its own name
# prefix, not the names it is about to capture, so it matches in both directions
# (`--only '^facet_ex'` must still enter the block whose prefix is `facet_`).
# Over-entering a block costs a container start; under-entering would silently
# capture nothing, so the bias is toward entering.
#
# The block prefix and `--only` are both conventionally `^prefix_` patterns.
# Their leading carets defeat a bare `[[ =~ ]]` for the exact-match case (a
# literal `^` in the string is not the regex anchor, so `[[ '^geo_' =~ ^geo_ ]]`
# is false), which silently skipped a block's container setup under the very
# `--only '^geo_'` form every appended block uses to re-capture itself -- the
# captures then ran against a container that was never started. The final
# string-prefix fallback (leading `^` stripped) covers the documented form
# without weakening the regex path for genuine regex `--only` values.
want_any() {
  [ -z "$ONLY" ] && return 0
  [[ $ONLY =~ $1 || $1 =~ $ONLY ]] && return 0
  local o="${ONLY#^}" b="${1#^}"
  [[ $o == "$b"* || $b == "$o"* ]]
}

# --- releasing a block's own container --------------------------------------
# Every block that stands up its own container used to leave it running, on the
# theory that a later run could reuse it. Twenty of them at ~500 MiB each is
# ~10 GiB, past what a default Docker Desktop gets (8.8 GiB here), so a full or
# filtered run OOMs partway down the script -- the blocks after the one that
# died capture nothing, silently, because each `cap*` helper's curl failure is
# swallowed into a status file. Releasing each block's container at the end of
# its block keeps the peak at the two reference containers plus one.
#
# KEEP_CONTAINERS=1 restores the old behaviour for interactive debugging, when
# you want to poke at a block's core after the run.
release() {  # release <container> <description>
  if [ -n "${KEEP_CONTAINERS:-}" ]; then
    echo "$2 left in place on '$1' (docker rm -f $1 to stop)"
  else
    docker rm -f "$1" >/dev/null 2>&1 || true
    echo "$2 captured; released container '$1'"
  fi
}

# Manifest paths are variables so that a filtered run can send its rows somewhere
# else entirely. Every append site below writes to `$MANIFEST` or
# `$MANIFEST_ERRORS`, never to a literal path.
MANIFEST="$HERE/manifest.tsv"
MANIFEST_ERRORS="$HERE/manifest-errors.tsv"

# A filtered run writes its rows into two scratch manifests and merges them into
# the committed ones when the run finishes: a row whose name already exists is
# replaced in place, so row order never churns, and a new row is appended.
#
# The committed manifests are never emptied, which is the whole point of doing it
# this way. An earlier version saved copies aside, truncated the real files, and
# restored them from a trap; interrupting that run left both manifests at zero
# rows, because bash defers a signal trap until the current foreground command
# returns and a capture run spends minutes inside `docker` and `curl` children.
# Signal traps cannot make truncation safe, so nothing gets truncated: an
# interrupted filtered run loses only the scratch rows, and the committed
# manifests are byte-identical to what git has. The merge runs from an EXIT trap
# so that a `set -e` failure mid-run still records what was actually captured.
ONLY_SCRATCH=""
only_merge() {
  local scratch pair committed captured
  [ -n "$ONLY_SCRATCH" ] && [ -d "$ONLY_SCRATCH" ] || return 0
  # Clear first: this must be idempotent, EXIT can reach it more than once.
  scratch=$ONLY_SCRATCH
  ONLY_SCRATCH=""
  for pair in "manifest.tsv:$HERE/manifest.tsv" "manifest-errors.tsv:$HERE/manifest-errors.tsv"; do
    captured="$scratch/${pair%%:*}"
    committed="${pair#*:}"
    [ -s "$captured" ] || continue
    awk -F'\t' '
      NR == FNR { if (NF) { captured[$1] = $0; order[++cnt] = $1 } next }
      { if ($1 in captured) { print captured[$1]; seen[$1] = 1 } else print }
      END {
        for (i = 1; i <= cnt; i++) {
          n = order[i]
          if (!(n in seen)) { print captured[n]; seen[n] = 1 }
        }
      }
    ' "$captured" "$committed" > "$captured.merged"
    mv "$captured.merged" "$committed"
    echo "capture.sh: merged $(grep -c . "$captured") row(s) into $(basename "$committed")"
  done
  rm -rf "$scratch"
}

if [ -n "$ONLY" ]; then
  ONLY_SCRATCH=$(mktemp -d)
  MANIFEST="$ONLY_SCRATCH/manifest.tsv"
  MANIFEST_ERRORS="$ONLY_SCRATCH/manifest-errors.tsv"
  : > "$MANIFEST"; : > "$MANIFEST_ERRORS"
  trap only_merge EXIT
  mkdir -p "$OUT"
  echo "capture.sh: --only '$ONLY' -- keeping existing fixtures and manifest rows"
else
  rm -rf "$OUT"; mkdir -p "$OUT"
  : > "$MANIFEST"
fi

# --- Solr up ---------------------------------------------------------------
if ! docker ps --format '{{.Names}}' | grep -qx "$CONTAINER"; then
  docker rm -f "$CONTAINER" >/dev/null 2>&1 || true
  docker run -d --name "$CONTAINER" -p "$SOLR_PORT:8983" solr:9 solr-precreate "$CORE" >/dev/null
fi

echo -n "waiting for solr"
for _ in $(seq 60); do
  if curl -sf "$SOLR/$CORE/admin/ping?wt=json" >/dev/null 2>&1; then echo " ok"; break; fi
  echo -n "."; sleep 1
done

# --- Schema: the tracer-bullet fields --------------------------------------
# `id` already exists in the _default configset as the string uniqueKey.
# Tolerant: re-runs against a live container hit "field already exists", which is fine.
# A genuinely broken schema surfaces on the corpus POST below, which is not tolerant.
curl -s "$SOLR/$CORE/schema" -H 'Content-Type: application/json' -d '{
  "add-field": [
    {"name":"body",     "type":"text_en", "indexed":true, "stored":true},
    {"name":"category", "type":"string",  "indexed":true, "stored":true,
     "docValues":true, "multiValued":true}
  ],
  "add-dynamic-field": {
    "name":"tm_X3b_en_*", "type":"text_en", "indexed":true,
    "stored":false, "multiValued":true
  }
}' >/dev/null

# --- Corpus ----------------------------------------------------------------
# The unstored dotted dynamic values back issue #177's captures at the end of
# this script. Seeding them here avoids overwriting existing IDs later, which
# would change live segment/doc order and deleted-term statistics for old rows.
curl -sf "$SOLR/$CORE/update?commit=true" -H 'Content-Type: application/json' -d '[
  {"id":"doc1","body":"the quick brown fox jumps over the lazy dog","category":["animals","classic"],"tm_X3b_en_a.b":["gamma"]},
  {"id":"doc2","body":"a lazy afternoon in the garden","category":["garden"],"tm_X3b_en_.leading":["gamma"]},
  {"id":"doc3","body":"quick thinking saves the day","category":["misc","classic"],"tm_X3b_en_trailing.":["gamma"]},
  {"id":"doc4","body":"dogs and cats living together","category":["animals"],"tm_X3b_en_a..b":["gamma"]},
  {"id":"doc5","body":"nothing much here at all"}
]' >/dev/null

# --- Capture ---------------------------------------------------------------
cap() {  # cap <name> <path-with-query>
  local name=$1 path=$2
  want "$name" || return 0
  # -g: disable URL globbing, or curl chokes on '[' in the bad-syntax fixture
  curl -sg "$SOLR/$CORE/$path" -o "$OUT/$name.json" -w '%{http_code}' > "$OUT/$name.status"
  printf '%s\t%s\t%s\n' "$name" "$(cat "$OUT/$name.status")" "$path" >> "$MANIFEST"
  rm -f "$OUT/$name.status"
}

cap ping                'admin/ping?wt=json'

# happy path
cap select_all          'select?q=*:*&rows=10&wt=json'
cap select_term         'select?q=lazy&df=body&fl=id,body&rows=2&start=0&wt=json'
cap select_paged        'select?q=*:*&rows=2&start=3&wt=json'
cap select_fq           'select?q=*:*&fq=category:animals&wt=json'
cap select_fq_multi     'select?q=*:*&fq=category:animals&fq=category:classic&wt=json'
cap select_sort         'select?q=*:*&sort=id+desc&rows=3&wt=json'

# faceting — the fiddly envelope
cap facet_basic         'select?q=*:*&rows=0&facet=true&facet.field=category&wt=json'
cap facet_mincount      'select?q=*:*&rows=0&facet=true&facet.field=category&facet.mincount=2&wt=json'
cap facet_limit         'select?q=*:*&rows=0&facet=true&facet.field=category&facet.limit=1&wt=json'
cap facet_missing       'select?q=*:*&rows=0&facet=true&facet.field=category&facet.missing=true&wt=json'
cap facet_sort_index    'select?q=*:*&rows=0&facet=true&facet.field=category&facet.sort=index&wt=json'
cap facet_query         'select?q=*:*&rows=0&facet=true&facet.query=category:animals&wt=json'
cap facet_json_nl_map   'select?q=*:*&rows=0&facet=true&facet.field=category&json.nl=map&wt=json'

# edges
cap select_zero         'select?q=zzzznope&df=body&wt=json'
cap facet_zero          'select?q=zzzznope&df=body&rows=0&facet=true&facet.field=category&wt=json'
cap facet_all_filtered  'select?q=*:*&rows=0&facet=true&facet.field=category&facet.mincount=99&wt=json'
cap select_past_end     'select?q=*:*&rows=10&start=999&wt=json'
cap select_fl_missing   'select?q=*:*&rows=1&fl=id,nosuchfield&wt=json'
cap select_rows_zero    'select?q=*:*&rows=0&wt=json'
cap select_doc_no_field 'select?q=id:doc5&wt=json'

# errors
cap err_unknown_field   'select?q=nosuchfield:x&wt=json'
cap err_bad_syntax      'select?q=*:*&fq=category:[unclosed&wt=json'
cap err_bad_sort        'select?q=*:*&sort=body+desc&wt=json'
cap err_unknown_param   'select?q=*:*&notaparam=1&wt=json'

# --- error shapes, issue #11 -----------------------------------------------
# Plain core-relative GET: goes in manifest.tsv like every other query, so the
# differential harness picks it up for free.
cap err_missing_q       'select?wt=json'

# The rest are not core-relative GETs (other core, POST body, non-GET method),
# so they cannot live in manifest.tsv without breaking its "core-relative GET"
# contract. Separate index: name, status, method, url-after-/solr/, body.
capx() {  # capx <name> <method> <url-after-/solr/> [body]
  local name=$1 method=$2 suffix=$3 body=${4-}
  want "$name" || return 0
  if [ -n "$body" ]; then
    curl -sg -X "$method" "$SOLR/$suffix" -H 'Content-Type: application/json' -d "$body" \
      -o "$OUT/$name.json" -w '%{http_code}' > "$OUT/$name.status"
  else
    curl -sg -X "$method" "$SOLR/$suffix" \
      -o "$OUT/$name.json" -w '%{http_code}' > "$OUT/$name.status"
  fi
  printf '%s\t%s\t%s\t%s\t%s\n' \
    "$name" "$(cat "$OUT/$name.status")" "$method" "$suffix" "$body" \
    >> "$MANIFEST_ERRORS"
  rm -f "$OUT/$name.status"
}
: > "$MANIFEST_ERRORS"

capx err_missing_core    GET    "nosuchcore/select?q=*:*&wt=json"
capx err_update_bad_json POST   "$CORE/update?commit=true&wt=json" '{not json'
capx err_select_delete   DELETE "$CORE/select?q=*:*&wt=json"
capx err_update_put      PUT    "$CORE/update?wt=json" '[]'
# --- Doc-level field handling (issue #10) ----------------------------------
# The _default configset is *schemaless*: `update.autoCreateFields` defaults to
# true, so an unknown document field is silently added to the schema (as
# text_general) instead of being rejected. That is a configset behaviour, not a
# Solr-core one, and it is explicitly out of scope for Wayfinder (PRD §3: no
# runtime schema mutation). So capture both sides:
#   update_unknown_field_schemaless -> what the _default configset does (200)
#   update_unknown_field_strict     -> what a non-schemaless Solr does (400),
#                                      which is the behaviour Wayfinder matches
# The strict side needs -Dupdate.autoCreateFields=false, a JVM-wide property, so
# it runs in its own container on another port.
# Indexed in manifest-errors.tsv, not manifest.tsv: these are POSTs with a body,
# and manifest.tsv's contract is "core-relative GET" — the differential harness
# (issue #1) GETs every row in it verbatim. Same 5-column format as capx above,
# plus a 6th column for the base URL, since the strict side runs on another port.
cap_post() {  # cap_post <name> <path-with-query> <json-body> [base-url] [core]
  local name=$1 path=$2 body=$3 base=${4:-$SOLR} core=${5:-$CORE}
  want "$name" || return 0
  curl -sg "$base/$core/$path" -H 'Content-Type: application/json' -d "$body" \
    -o "$OUT/$name.json" -w '%{http_code}' > "$OUT/$name.status"
  printf '%s\t%s\t%s\t%s\t%s\t%s\n' \
    "$name" "$(cat "$OUT/$name.status")" POST "$core/$path" "$body" "$base" \
    >> "$MANIFEST_ERRORS"
  rm -f "$OUT/$name.status"
}

# Schemaless side. This runs on its OWN core, not `content`, and that is load
# bearing: the probe's whole point is that Solr auto-adds the unknown field, and
# Solr cannot then delete it (an auto-generated copy-field directive references
# it). Running it on `content` permanently adds `nosuchfield` to the reference
# core's schema, after which `err_unknown_field`'s query (`q=nosuchfield:x`)
# resolves and returns 200 instead of 400 — so the container could no longer
# reproduce the very fixtures this script had just captured, and the live
# differential mode failed on it (issue #26). Deleting the probe doc is not
# enough; the schema change is what persists.
PROBE_CORE=schemaless_probe
# `solr create` rather than the CREATE admin API: the API needs an instanceDir
# that already holds a configset, which a fresh name does not.
docker exec "$CONTAINER" solr create -c "$PROBE_CORE" >/dev/null 2>&1 || true
cap_post update_unknown_field_schemaless 'update?commit=true' \
  '[{"id":"probe_unknown_field","body":"probe","nosuchfield":"x"}]' \
  "$SOLR" "$PROBE_CORE"

# Strict side, its own container/port.
STRICT_CONTAINER=wayfinder-solr-ref-strict
STRICT_SOLR=http://localhost:8984/solr
if ! docker ps --format '{{.Names}}' | grep -qx "$STRICT_CONTAINER"; then
  docker rm -f "$STRICT_CONTAINER" >/dev/null 2>&1 || true
  docker run -d --name "$STRICT_CONTAINER" -p 8984:8983 \
    -e SOLR_OPTS=-Dupdate.autoCreateFields=false \
    solr:9 solr-precreate "$CORE" >/dev/null
fi
echo -n "waiting for strict solr"
for _ in $(seq 60); do
  if curl -sf "$STRICT_SOLR/$CORE/admin/ping?wt=json" >/dev/null 2>&1; then echo " ok"; break; fi
  echo -n "."; sleep 1
done
curl -s "$STRICT_SOLR/$CORE/schema" -H 'Content-Type: application/json' -d '{
  "add-field": [
    {"name":"body",     "type":"text_en", "indexed":true, "stored":true},
    {"name":"category", "type":"string",  "indexed":true, "stored":true,
     "docValues":true, "multiValued":true}
  ]
}' >/dev/null
cap_post update_unknown_field_strict 'update?commit=true' \
  '[{"id":"probe_unknown_field","body":"probe","nosuchfield":"x"}]' "$STRICT_SOLR"

# --- sort parameter (issue #2) ----------------------------------------------
# Appended block; nothing above is edited, and the shared schema / 5-doc corpus
# are deliberately untouched so every pre-existing fixture stays byte-identical.
#
# The corpus only offers three sortable-ish fields: `id` (string, single-valued,
# docValues via the _default `string` type), `body` (text_en, no docValues) and
# `category` (string, docValues, *multiValued*). That is enough for every case in
# the issue without adding a field:
#   - direction:      sort=id asc / id desc
#   - multi-clause:   score <dir>,id <dir> — with q=*:* every doc scores the same,
#                     so the second clause alone decides the order, which is what
#                     proves multi-clause parsing is honoured end to end
#   - score:          q=lazy scores doc1/doc2 differently, so asc vs desc is visible
#   - pagination:     sort + start/rows
#   - multiValued:    sorting on `category` is NOT an error in Solr 9 — it is a
#                     200 that uses Lucene's SortedSetSortField selector (min
#                     value for asc, max for desc), missing values last. Captured
#                     both directions because that is the only way to see it.
#   - error paths:    a non-fast field buried among valid clauses, a junk
#                     direction token, and a missing direction token (the last two
#                     were captured to find out what Solr does; both are 400s)
cap select_sort_asc            'select?q=*:*&sort=id+asc&rows=3&wt=json'
cap select_sort_score_all      'select?q=*:*&sort=score+desc&rows=10&wt=json'
cap select_sort_score_desc     'select?q=lazy&df=body&sort=score+desc&rows=5&wt=json'
cap select_sort_score_asc      'select?q=lazy&df=body&sort=score+asc&rows=5&wt=json'
cap select_sort_multi_asc      'select?q=*:*&sort=score+desc,id+asc&rows=5&wt=json'
cap select_sort_multi_desc     'select?q=*:*&sort=score+desc,id+desc&rows=5&wt=json'
cap select_sort_paged          'select?q=*:*&sort=id+desc&rows=2&start=2&wt=json'
cap select_sort_paged_past_end 'select?q=*:*&sort=id+desc&rows=2&start=99&wt=json'
cap select_sort_mv_asc         'select?q=*:*&sort=category+asc&wt=json'
cap select_sort_mv_desc        'select?q=*:*&sort=category+desc&wt=json'
cap err_sort_bad_clause_among_good 'select?q=*:*&sort=id+asc,body+desc&wt=json'
cap err_sort_bad_direction     'select?q=*:*&sort=id+sideways&wt=json'
cap err_sort_no_direction      'select?q=*:*&sort=id&wt=json'

# Which error wins when a sort spec has more than one problem. Captured over two
# review rounds, because issue #2 twice asserted a check order on inference and
# was twice wrong. Each fixture below establishes exactly one thing -- read the
# scope carefully, it is narrower than it looks:
#   - score+sideways:        the direction check fails. pos=5 (past the field name).
#                            Establishes: score is NOT exempt from the direction
#                            check. It says nothing about field resolution -
#                            under direction-first a bad direction errors either
#                            way. The special-casing is established by
#                            select_sort_score_* returning 200 and ranking.
#   - body+desc,id+sideways: an EARLIER clause's field error beats a LATER clause's
#                            direction error. Establishes: clause-by-clause, left to
#                            right, stopping at the first bad clause. It says
#                            NOTHING about the order of checks *within* a clause --
#                            clause ordering alone explains it.
#   - body+sideways:         one clause, bad in BOTH ways (non-docValues field AND a
#                            junk direction). The only spec that separates the two
#                            within-clause orders, and Solr answers the DIRECTION
#                            error. Establishes: within a clause the direction is
#                            checked BEFORE the field is resolved.
cap err_sort_score_bad_direction   'select?q=*:*&sort=score+sideways&wt=json'
cap err_sort_field_before_direction 'select?q=*:*&sort=body+desc,id+sideways&wt=json'
cap err_sort_direction_before_field 'select?q=*:*&sort=body+sideways&wt=json'

echo
column -t -s $'\t' "$MANIFEST"
echo
column -t -s $'\t' "$MANIFEST_ERRORS"
echo
echo "captured $(wc -l < "$MANIFEST" | tr -d ' ') responses -> $OUT"
echo "solr still running as '$CONTAINER' (docker rm -f $CONTAINER to stop)"
echo "strict solr still running as '$STRICT_CONTAINER' (docker rm -f $STRICT_CONTAINER to stop)"

# --- Faceting completion (issue #3) ----------------------------------------
# Everything below is appended at the end per CLAUDE.md, so re-running the
# script leaves every pre-existing manifest row byte-identical and only adds
# rows at the tail.
#
# Main-core captures: plain core-relative GETs against the untouched 5-doc
# corpus, so they belong in manifest.tsv and the differential harness picks
# them up for free.

# Multiple facet.field: `id` is a string field with docValues=true in the
# _default configset, so it facets alongside `category`.
cap facet_multi_field       'select?q=*:*&rows=0&facet=true&facet.field=category&facet.field=id&wt=json'
cap facet_json_nl_map_multi 'select?q=*:*&rows=0&facet=true&facet.field=category&facet.field=id&json.nl=map&wt=json'

# The dictionary-enumeration property at two different hit-set sizes: terms
# reachable only by non-matching docs must still appear, at 0.
cap facet_subset            'select?q=id:doc2&rows=0&facet=true&facet.field=category&wt=json'

# facet.sort tie-break, on a hit set where count order and term order differ
# (`quick` matches doc1+doc3 -> classic 2, animals 1, misc 1, garden 0).
cap facet_sort_count_tiebreak 'select?q=quick&df=body&rows=0&facet=true&facet.field=category&facet.sort=count&wt=json'
cap facet_sort_index_subset   'select?q=quick&df=body&rows=0&facet=true&facet.field=category&facet.sort=index&wt=json'

# facet.missing is hit-set-based: doc5 (the value-less doc) is outside this
# hit set, so the null bucket must be 0, not 1.
cap facet_missing_no_hit    'select?q=id:doc2&rows=0&facet=true&facet.field=category&facet.missing=true&wt=json'

# facet.limit boundaries.
cap facet_limit_zero        'select?q=*:*&rows=0&facet=true&facet.field=category&facet.limit=0&wt=json'
cap facet_limit_unlimited   'select?q=*:*&rows=0&facet=true&facet.field=category&facet.limit=-1&wt=json'

# facet.mincount=1 drops the zero-count dictionary terms.
cap facet_mincount_one      'select?q=id:doc2&rows=0&facet=true&facet.field=category&facet.mincount=1&wt=json'

# Repeatable facet.query, a facet query matching nothing (key present at 0),
# and a facet query intersected with q + fq.
cap facet_query_multi       'select?q=*:*&rows=0&facet=true&facet.query=category:animals&facet.query=category:garden&wt=json'
cap facet_query_zero        'select?q=*:*&rows=0&facet=true&facet.query=category:nosuchvalue&wt=json'
cap facet_query_with_fq     'select?q=*:*&fq=category:classic&rows=0&facet=true&facet.query=category:animals&wt=json'

# --- Premise check: what does Solr do for a facet it cannot build? ---------
# These go in manifest-errors.tsv, not manifest.tsv, even though they are
# core-relative GETs: Wayfinder deliberately diverges here (a hard 400 rather
# than a silent empty array), so putting them in manifest.tsv would demand a
# permanent EXPECTED_DIVERGENCES entry in the differential harness. They are
# captured as ground truth for the documented divergence, not as a target.
#   facet_non_docvalues_text      `body`: text_en, indexed, stored, no docValues
#   facet_non_docvalues_text_enum the same field with facet.method=enum
#   facet_unknown_field           a field that does not exist at all
capx facet_non_docvalues_text      GET "$CORE/select?q=*:*&rows=0&facet=true&facet.field=body&wt=json"
capx facet_non_docvalues_text_enum GET "$CORE/select?q=*:*&rows=0&facet=true&facet.field=body&facet.method=enum&wt=json"
capx facet_unknown_field           GET "$CORE/select?q=*:*&rows=0&facet=true&facet.field=nosuchfield&wt=json"

# --- facet.range: a second core, same container ----------------------------
# facet.range needs a numeric or date field. Adding one to the `content` core
# would rewrite ground truth for every doc-returning fixture, so the range
# corpus lives in its own core with its own 4 docs. Captured with capx (not a
# main-core GET, so not a manifest.tsv row).
#   views   pint,   indexed+stored+docValues  -> numeric range facet
#   created pdate,  indexed+stored+docValues  -> date range facet
#   note    string, stored only (no indexed, no docValues) -> unfacetable
RANGE_CORE=facets
if ! curl -sf "$SOLR/admin/cores?action=STATUS&core=$RANGE_CORE&wt=json" \
     | grep -q "\"name\":\"$RANGE_CORE\""; then
  docker exec "$CONTAINER" solr create -c "$RANGE_CORE" >/dev/null
fi
curl -s "$SOLR/$RANGE_CORE/schema" -H 'Content-Type: application/json' -d '{
  "add-field": [
    {"name":"views",   "type":"pint",   "indexed":true, "stored":true, "docValues":true},
    {"name":"created", "type":"pdate",  "indexed":true, "stored":true, "docValues":true},
    {"name":"note",    "type":"string", "indexed":false,"stored":true, "docValues":false}
  ]
}' >/dev/null
curl -sf "$SOLR/$RANGE_CORE/update?commit=true" -H 'Content-Type: application/json' -d '[
  {"id":"r1","views":5, "created":"2020-01-02T00:00:00Z","note":"alpha"},
  {"id":"r2","views":15,"created":"2020-01-03T00:00:00Z","note":"beta"},
  {"id":"r3","views":25,"created":"2020-01-03T00:00:00Z","note":"alpha"},
  {"id":"r4","views":35,"created":"2020-01-05T00:00:00Z"}
]' >/dev/null

capx facet_range_numeric GET \
  "$RANGE_CORE/select?q=*:*&rows=0&facet=true&facet.range=views&facet.range.start=0&facet.range.end=40&facet.range.gap=10&wt=json"
capx facet_range_date GET \
  "$RANGE_CORE/select?q=*:*&rows=0&facet=true&facet.range=created&facet.range.start=2020-01-01T00:00:00Z&facet.range.end=2020-01-06T00:00:00Z&facet.range.gap=%2B1DAY&wt=json"
capx facet_range_json_nl_map GET \
  "$RANGE_CORE/select?q=*:*&rows=0&facet=true&facet.range=views&facet.range.start=0&facet.range.end=40&facet.range.gap=10&json.nl=map&wt=json"
# A stored-only field: neither indexed nor docValues, the case where Solr has
# nothing at all to read.
capx facet_stored_only_field GET \
  "$RANGE_CORE/select?q=*:*&rows=0&facet=true&facet.field=note&wt=json"

echo
column -t -s $'\t' "$MANIFEST"
echo
column -t -s $'\t' "$MANIFEST_ERRORS"
echo
echo "captured $(wc -l < "$MANIFEST" | tr -d ' ') manifest.tsv rows -> $OUT"
echo "range-facet core '$RANGE_CORE' left in place on '$CONTAINER'"

# --- JSON object key order (issue #25) -------------------------------------
# Appended at the end per CLAUDE.md; adds rows only at the tail of
# manifest-errors.tsv and never re-captures an existing fixture.
#
# Its OWN container on its OWN port, and its own core. Three reasons, all
# learned the hard way:
#   1. The `facets` core's fixtures are all `q=*:*&rows=0` captures, so adding
#      docs to that core to widen the range would move `numFound` in every one
#      of them -- re-capturing ground truth as a side effect, which the
#      compatibility contract forbids.
#   2. A wide range (0-200 by 10) and a term distribution where
#      count-descending and alphabetical order differ both need their own
#      corpus.
#   3. Issue #24 was capturing against `wayfinder-solr-ref` on 8983 while this
#      block was written. Two concurrent runs against one container is how
#      fixtures got churned earlier in this project.
# Same precedent as `wayfinder-solr-ref-strict` on 8984 and the
# `schemaless_probe` core: separate container, separate port, separate core.
KEYORDER_CONTAINER=wayfinder-solr-25
KEYORDER_SOLR=http://localhost:8986/solr
KEYORDER_CORE=keyorder
if ! docker ps --format '{{.Names}}' | grep -qx "$KEYORDER_CONTAINER"; then
  docker rm -f "$KEYORDER_CONTAINER" >/dev/null 2>&1 || true
  docker run -d --name "$KEYORDER_CONTAINER" -p 8986:8983 solr:9 \
    solr-precreate "$KEYORDER_CORE" >/dev/null
fi
echo -n "waiting for key-order solr"
for _ in $(seq 90); do
  if curl -sf "$KEYORDER_SOLR/$KEYORDER_CORE/admin/ping?wt=json" >/dev/null 2>&1; then
    echo " ok"; break
  fi
  echo -n "."; sleep 1
done

# `views`: pint with docValues -> facet.range over 0-200 by 10, so the bucket
# keys ("0","10",...,"100","110",...) order differently numerically than
# alphabetically ("100" sorts before "20").
# `tag`: string, docValues, multiValued -> facet.field where count-descending
# (apple 5, zebra 5, mango 2, banana 1) differs from alphabetical (apple,
# banana, mango, zebra).
curl -s "$KEYORDER_SOLR/$KEYORDER_CORE/schema" -H 'Content-Type: application/json' -d '{
  "add-field": [
    {"name":"views", "type":"pint",  "indexed":true, "stored":true, "docValues":true},
    {"name":"tag",   "type":"string","indexed":true, "stored":true,
     "docValues":true, "multiValued":true}
  ]
}' >/dev/null
curl -sf "$KEYORDER_SOLR/$KEYORDER_CORE/update?commit=true" -H 'Content-Type: application/json' -d '[
  {"id":"k1","views":5,  "tag":["zebra","apple"]},
  {"id":"k2","views":15, "tag":["zebra","apple"]},
  {"id":"k3","views":45, "tag":["zebra","mango"]},
  {"id":"k4","views":95, "tag":["zebra","apple"]},
  {"id":"k5","views":105,"tag":["mango","banana"]},
  {"id":"k6","views":155,"tag":["apple"]},
  {"id":"k7","views":195,"tag":["apple"]},
  {"id":"k8","views":125,"tag":["zebra"]}
]' >/dev/null

# Own capture helper: `capx` hardcodes the main `$SOLR` base, and these live on
# another host:port. Sixth column records the base URL, exactly as the
# strict-container rows in manifest-errors.tsv already do.
capk() {  # capk <name> <path-after-core>
  local name=$1 suffix=$2
  want "$name" || return 0
  curl -sg "$KEYORDER_SOLR/$KEYORDER_CORE/$suffix" \
    -o "$OUT/$name.json" -w '%{http_code}' > "$OUT/$name.status"
  printf '%s\t%s\t%s\t%s\t%s\t%s\n' \
    "$name" "$(cat "$OUT/$name.status")" GET "$KEYORDER_CORE/$suffix" "" "$KEYORDER_SOLR" \
    >> "$MANIFEST_ERRORS"
  rm -f "$OUT/$name.status"
}

# --- facet.field on numeric / date columns (issue #24) ----------------------
# Appended block; nothing above is edited.
#
# Why its own container and port, rather than reusing `$CONTAINER`'s `facets`
# core: this block was written while issue #25 was capturing concurrently
# against `wayfinder-solr-ref` on 8983, and the top of this script *rebuilds*
# that container destructively. Two concurrent runs against one container is how
# fixtures got churned earlier in this project. Same precedent as
# `wayfinder-solr-ref-strict` on 8984 -- a self-contained container, core,
# schema and corpus, so this block cannot perturb the reference core or the
# fixtures already captured from it (issue #26's lesson: never leave the
# reference core unable to reproduce its own captures). It is *not* runnable
# standalone, though: it uses `$OUT`/`$HERE` set at the top of the script, and
# `capf` appends to `manifest-errors.tsv` unconditionally, so re-running just
# this block would duplicate its eleven rows there. Run the whole script.
#
# The core, schema and corpus are identical to the `facets` core the issue #3
# block builds above, so these fixtures are ground truth for the same 4-doc
# corpus `tests/faceting.rs::range_app` mirrors.
FACET_CONTAINER=wayfinder-solr-24
FACET_SOLR=http://localhost:8985/solr
FACET_CORE=facets
if ! docker ps --format '{{.Names}}' | grep -qx "$FACET_CONTAINER"; then
  docker rm -f "$FACET_CONTAINER" >/dev/null 2>&1 || true
  docker run -d --name "$FACET_CONTAINER" -p 8985:8983 \
    solr:9 solr-precreate "$FACET_CORE" >/dev/null
fi
echo -n "waiting for facet solr"
for _ in $(seq 60); do
  if curl -sf "$FACET_SOLR/$FACET_CORE/admin/ping?wt=json" >/dev/null 2>&1; then echo " ok"; break; fi
  echo -n "."; sleep 1
done
curl -s "$FACET_SOLR/$FACET_CORE/schema" -H 'Content-Type: application/json' -d '{
  "add-field": [
    {"name":"views",   "type":"pint",   "indexed":true, "stored":true, "docValues":true},
    {"name":"created", "type":"pdate",  "indexed":true, "stored":true, "docValues":true},
    {"name":"note",    "type":"string", "indexed":false,"stored":true, "docValues":false}
  ]
}' >/dev/null
curl -sf "$FACET_SOLR/$FACET_CORE/update?commit=true" -H 'Content-Type: application/json' -d '[
  {"id":"r1","views":5, "created":"2020-01-02T00:00:00Z","note":"alpha"},
  {"id":"r2","views":15,"created":"2020-01-03T00:00:00Z","note":"beta"},
  {"id":"r3","views":25,"created":"2020-01-03T00:00:00Z","note":"alpha"},
  {"id":"r4","views":35,"created":"2020-01-05T00:00:00Z"}
]' >/dev/null

# Same 5-column manifest-errors.tsv contract as `capx`, plus the base URL as a
# 6th column (the `update_unknown_field_*` precedent) so it is recorded which
# Solr answered. Not manifest.tsv: these are not `content`-relative GETs, so the
# differential harness must not GET them against the reference core.
capf() {  # capf <name> <url-after-/solr/>
  local name=$1 suffix=$2
  want "$name" || return 0
  curl -sg "$FACET_SOLR/$suffix" -o "$OUT/$name.json" -w '%{http_code}' > "$OUT/$name.status"
  printf '%s\t%s\t%s\t%s\t%s\t%s\n' \
    "$name" "$(cat "$OUT/$name.status")" GET "$suffix" "" "$FACET_SOLR" \
    >> "$MANIFEST_ERRORS"
  rm -f "$OUT/$name.status"
}

capk keyorder_range_wide_map \
  'select?q=*:*&rows=0&facet=true&facet.range=views&facet.range.start=0&facet.range.end=200&facet.range.gap=10&json.nl=map&wt=json'
capk keyorder_facet_field_map \
  'select?q=*:*&rows=0&facet=true&facet.field=tag&json.nl=map&wt=json'
capk keyorder_facet_field_map_index \
  'select?q=*:*&rows=0&facet=true&facet.field=tag&facet.sort=index&json.nl=map&wt=json'

release "$KEYORDER_CONTAINER" "key-order core '$KEYORDER_CORE'"

# The whole point of the issue: `views` has four distinct values but `q=id:r1`
# matches one document. If Solr enumerates the numeric term dictionary the way
# it does a string one, 15/25/35 come back at 0. `q=*:*` is the control -- it
# cannot distinguish enumeration from hit-set mapping, which is exactly why the
# subset capture is the one that decides.
capf facet_field_numeric_all    "$FACET_CORE/select?q=*:*&rows=0&facet=true&facet.field=views&wt=json"
capf facet_field_numeric_subset "$FACET_CORE/select?q=id:r1&rows=0&facet=true&facet.field=views&wt=json"

# The same question for a date column, and the rendering of a date facet term
# (Tantivy's date column is i64 nanos; Solr's key is an ISO-8601 string).
# `created` has three distinct values over four docs, so `q=*:*` also pins that
# a shared value counts 2.
capf facet_field_date_all       "$FACET_CORE/select?q=*:*&rows=0&facet=true&facet.field=created&wt=json"
capf facet_field_date_subset    "$FACET_CORE/select?q=id:r1&rows=0&facet=true&facet.field=created&wt=json"

# facet.sort on a numeric dictionary: `index` order separates numeric ordering
# (5,15,25,35) from the lexical ordering of the rendered keys (15,25,35,5), and
# `count` order over a hit set of one says where the zero-count terms land.
capf facet_field_numeric_sort_index "$FACET_CORE/select?q=id:r1&rows=0&facet=true&facet.field=views&facet.sort=index&wt=json"
capf facet_field_numeric_sort_count "$FACET_CORE/select?q=id:r1&rows=0&facet=true&facet.field=views&facet.sort=count&wt=json"

# The ordering question the one-hit captures above cannot answer: over the full
# corpus, is a numeric/date facet ordered by the *value* or by the lexical form
# of the rendered key? 5,15,25,35 vs "15","25","35","5" separates them, and
# `facet.sort=count` with four counts of 1 makes the tie-break visible too.
capf facet_field_numeric_sort_index_all "$FACET_CORE/select?q=*:*&rows=0&facet=true&facet.field=views&facet.sort=index&wt=json"
capf facet_field_date_sort_index_all    "$FACET_CORE/select?q=*:*&rows=0&facet=true&facet.field=created&facet.sort=index&wt=json"

# facet.mincount=1 must drop the zero-count numeric terms -- the control that
# says a zero-fill is a real enumeration being filtered, not a fabrication.
capf facet_field_numeric_mincount_one "$FACET_CORE/select?q=id:r1&rows=0&facet=true&facet.field=views&facet.mincount=1&wt=json"

# Numeric keys as object keys under json.nl=map.
capf facet_field_numeric_json_nl_map "$FACET_CORE/select?q=id:r1&rows=0&facet=true&facet.field=views&json.nl=map&wt=json"

# The control, and the reason the captures above can be trusted: `id` is a
# string field with docValues in the _default configset, so the SAME container,
# core, corpus and hit set (`q=id:r1`) must still enumerate for a *string*
# column -- r2/r3/r4 at 0. Without this, "the numeric facet reported no
# zero-count terms" is indistinguishable from a broken capture setup.
capf facet_field_string_control_subset "$FACET_CORE/select?q=id:r1&rows=0&facet=true&facet.field=id&wt=json"

# --- sort clause grammar + numeric/date sort corpus (issue #32) --------------
# Appended block; nothing above is edited. Same self-contained-container
# precedent as `wayfinder-solr-24` above (own container, own port, own core),
# and for the same reason: this was captured while issues #31 (8983) and #33
# (8988) ran concurrently, so it must not perturb the reference core or any
# other block's container. Like the #24 block it is not runnable standalone
# (`$OUT`/`$HERE`, and `caps` appends to manifest-errors.tsv unconditionally);
# run the whole script.
#
# Two questions this corpus is built to discriminate:
#   1. Clause grammar — is the comma optional (`sort=id asc category desc`
#      two clauses or an error?), where do dropped/extra tokens go, and is the
#      direction-error `pos` absolute in the spec or clause-relative
#      (`sort=id asc,id sideways`)? Findings 34-35.
#   2. Numeric/float/date missing-value placement — `s6` carries *negative*
#      values (and a pre-epoch date) precisely so "missing sorts as 0" is
#      distinguishable from "missing sorts first/last": the missing doc landing
#      *between* the negative value and the smallest positive one is what rules
#      both extremes out. Findings 36-37.
SORTDEBT_CONTAINER=wayfinder-solr-32
SORTDEBT_SOLR=http://localhost:8987/solr
SORTDEBT_CORE=sortdebt
if ! docker ps --format '{{.Names}}' | grep -qx "$SORTDEBT_CONTAINER"; then
  docker rm -f "$SORTDEBT_CONTAINER" >/dev/null 2>&1 || true
  docker run -d --name "$SORTDEBT_CONTAINER" -p 8987:8983 \
    solr:9 solr-precreate "$SORTDEBT_CORE" >/dev/null
fi
echo -n "waiting for sortdebt solr"
for _ in $(seq 60); do
  if curl -sf "$SORTDEBT_SOLR/$SORTDEBT_CORE/admin/ping?wt=json" >/dev/null 2>&1; then echo " ok"; break; fi
  echo -n "."; sleep 1
done
curl -s "$SORTDEBT_SOLR/$SORTDEBT_CORE/schema" -H 'Content-Type: application/json' -d '{
  "add-field": [
    {"name":"category","type":"string","indexed":true,"stored":true,"docValues":true},
    {"name":"views",   "type":"pint",  "indexed":true,"stored":true,"docValues":true},
    {"name":"weight",  "type":"pfloat","indexed":true,"stored":true,"docValues":true},
    {"name":"created", "type":"pdate", "indexed":true,"stored":true,"docValues":true},
    {"name":"nums",    "type":"pint",  "indexed":true,"stored":true,"docValues":true,"multiValued":true}
  ]
}' >/dev/null
curl -sf "$SORTDEBT_SOLR/$SORTDEBT_CORE/update?commit=true" -H 'Content-Type: application/json' -d '[
  {"id":"s1","category":"alpha",  "views":30,"weight":1.5,"created":"2021-03-01T00:00:00Z","nums":[10,90]},
  {"id":"s2","category":"beta",   "views":10,"weight":3.5,"created":"2021-01-01T00:00:00Z","nums":[50,60]},
  {"id":"s3","category":"gamma",  "views":20,"weight":2.5,"created":"2021-05-01T00:00:00Z","nums":[20,80]},
  {"id":"s4","category":"delta",              "weight":0.5,"created":"2021-02-01T00:00:00Z","nums":[70]},
  {"id":"s5","category":"epsilon","views":40},
  {"id":"s6","category":"zeta",   "views":-5,"weight":-1.5,"created":"1969-06-01T00:00:00Z","nums":[-10,5]}
]' >/dev/null

# Same 6-column manifest-errors.tsv contract as `capf` above: not the `content`
# core, so never manifest.tsv — the differential harness must not GET these
# against the reference core.
caps() {  # caps <name> <path-after-core>
  local name=$1 suffix=$2
  want "$name" || return 0
  curl -sg "$SORTDEBT_SOLR/$SORTDEBT_CORE/$suffix" \
    -o "$OUT/$name.json" -w '%{http_code}' > "$OUT/$name.status"
  printf '%s\t%s\t%s\t%s\t%s\t%s\n' \
    "$name" "$(cat "$OUT/$name.status")" GET "$SORTDEBT_CORE/$suffix" "" "$SORTDEBT_SOLR" \
    >> "$MANIFEST_ERRORS"
  rm -f "$OUT/$name.status"
}

# Clause grammar: is the comma optional, and what happens to extra tokens?
caps sort_clause_space_separated 'select?q=*:*&sort=id+asc+category+desc&wt=json'
caps sort_clause_trailing_garbage 'select?q=*:*&sort=id+asc+garbage&wt=json'
caps sort_clause_trailing_valid_field 'select?q=*:*&sort=id+asc+category&wt=json'
caps sort_clause_trailing_comma 'select?q=*:*&sort=id+asc,&wt=json'
caps sort_clause_leading_comma 'select?q=*:*&sort=,id+asc&wt=json'
caps sort_clause_double_comma 'select?q=*:*&sort=id+asc,,category+desc&wt=json'
caps sort_clause_empty 'select?q=*:*&sort=&wt=json'
caps sort_clause_space_before_comma 'select?q=*:*&sort=id+asc+,+category+desc&wt=json'
caps sort_clause_space_after_comma 'select?q=*:*&sort=id+asc,+category+desc&wt=json'

# Multi-clause pos: absolute within the spec, or clause-relative?
caps err_sort_second_clause_bad_direction 'select?q=*:*&sort=id+asc,id+sideways&wt=json'
caps err_sort_second_clause_no_direction 'select?q=*:*&sort=id+asc,category&wt=json'
# And pos under leading whitespace.
caps err_sort_leading_whitespace 'select?q=*:*&sort=%20%20id+sideways&wt=json'

# String (SortedSet) sort on this corpus, for the multi-segment test in
# tests/sort.rs: category values are arranged so the correct asc order
# interleaves the two commit batches the test uses, which is what makes a
# raw-cross-segment-ordinal comparison visibly wrong.
caps sort_string_asc 'select?q=*:*&sort=category+asc&fl=id,category&wt=json'
caps sort_string_desc 'select?q=*:*&sort=category+desc&fl=id,category&wt=json'

# Numeric / float / date sort, missing values in both directions.
caps sort_int_asc 'select?q=*:*&sort=views+asc&fl=id,views&wt=json'
caps sort_int_desc 'select?q=*:*&sort=views+desc&fl=id,views&wt=json'
caps sort_float_asc 'select?q=*:*&sort=weight+asc&fl=id,weight&wt=json'
caps sort_float_desc 'select?q=*:*&sort=weight+desc&fl=id,weight&wt=json'
caps sort_date_asc 'select?q=*:*&sort=created+asc&fl=id,created&wt=json'
caps sort_date_desc 'select?q=*:*&sort=created+desc&fl=id,created&wt=json'

# Min/max selector on a multiValued numeric field (`nums` values are arranged
# so the desc order is *not* the reverse of the asc order — that asymmetry is
# what proves the selector, not just a direction flip).
caps sort_mv_int_asc 'select?q=*:*&sort=nums+asc&fl=id,nums&wt=json'
caps sort_mv_int_desc 'select?q=*:*&sort=nums+desc&fl=id,nums&wt=json'

echo
column -t -s $'\t' "$MANIFEST_ERRORS"
echo
release "$FACET_CONTAINER" "numeric/date facet.field core '$FACET_CORE'"
release "$SORTDEBT_CONTAINER" "sort-debt core '$SORTDEBT_CORE'"

# --- facet debt: float/date rendering, unpinned semantics, error precedence --
# (issues #33 / #30.) Appended block; nothing above is edited.
#
# Own container on its own port, per the `wayfinder-solr-24` precedent above:
# this block was written while issues #31 (canonical container, 8983) and #32
# (8987) were running concurrently, and the top of this script rebuilds the
# reference container destructively. Same caveat as the #24 block: NOT runnable
# standalone — it uses `$OUT`/`$HERE` from the top of the script, and `capd`
# appends to `manifest-errors.tsv` unconditionally, so re-running just this
# block duplicates its rows. Run the whole script.
#
# The core is its own (`facets33`): the debt items need field types the
# `facets` core lacks (`pdouble`, `pfloat`, a millisecond-precision `pdate`,
# a single-valued docValues string with gaps for `facet.missing`).
DEBT_CONTAINER=wayfinder-solr-33
DEBT_SOLR=http://localhost:8988/solr
DEBT_CORE=facets33
if ! docker ps --format '{{.Names}}' | grep -qx "$DEBT_CONTAINER"; then
  docker rm -f "$DEBT_CONTAINER" >/dev/null 2>&1 || true
  docker run -d --name "$DEBT_CONTAINER" -p 8988:8983 \
    solr:9 solr-precreate "$DEBT_CORE" >/dev/null
fi
echo -n "waiting for facet-debt solr"
for _ in $(seq 60); do
  if curl -sf "$DEBT_SOLR/$DEBT_CORE/admin/ping?wt=json" >/dev/null 2>&1; then echo " ok"; break; fi
  echo -n "."; sleep 1
done
curl -s "$DEBT_SOLR/$DEBT_CORE/schema" -H 'Content-Type: application/json' -d '{
  "add-field": [
    {"name":"views", "type":"pint",    "indexed":true, "stored":true, "docValues":true},
    {"name":"price", "type":"pdouble", "indexed":true, "stored":true, "docValues":true},
    {"name":"rating","type":"pfloat",  "indexed":true, "stored":true, "docValues":true},
    {"name":"stamp", "type":"pdate",   "indexed":true, "stored":true, "docValues":true},
    {"name":"tag",   "type":"string",  "indexed":true, "stored":true, "docValues":true},
    {"name":"note",  "type":"string",  "indexed":false,"stored":true, "docValues":false}
  ]
}' >/dev/null
# `price` mixes integral (5.0, 12.0) and fractional (7.5, 0.25) doubles so the
# rendered key settles `"5"` vs `"5.0"`. `stamp` has two values inside the SAME
# second differing only in milliseconds (.123 vs .456) so millisecond rendering
# and chronological-vs-lexical ordering are both discriminated. `tag` is absent
# from r4/r5 so `facet.missing` has a 2-doc null bucket.
curl -sf "$DEBT_SOLR/$DEBT_CORE/update?commit=true" -H 'Content-Type: application/json' -d '[
  {"id":"r1","views":5, "price":5.0,  "rating":5.0,"stamp":"2020-01-02T00:00:00.123Z","tag":"apple", "note":"alpha"},
  {"id":"r2","views":15,"price":7.5,  "rating":7.5,"stamp":"2020-01-02T00:00:00.456Z","tag":"apple"},
  {"id":"r3","views":25,"price":5.0,  "rating":5.0,"stamp":"2020-01-03T12:34:56.789Z","tag":"banana"},
  {"id":"r4","views":35,"price":12.0, "stamp":"2020-01-05T00:00:00Z"},
  {"id":"r5","views":45,"price":0.25}
]' >/dev/null

# Same 6-column manifest-errors.tsv contract as `capf` above.
capd() {  # capd <name> <url-after-/solr/>
  local name=$1 suffix=$2
  want "$name" || return 0
  curl -sg "$DEBT_SOLR/$suffix" -o "$OUT/$name.json" -w '%{http_code}' > "$OUT/$name.status"
  printf '%s\t%s\t%s\t%s\t%s\t%s\n' \
    "$name" "$(cat "$OUT/$name.status")" GET "$suffix" "" "$DEBT_SOLR" \
    >> "$MANIFEST_ERRORS"
  rm -f "$OUT/$name.status"
}

# --- error precedence (#30): one broken family at a time, then every pair, ---
# --- then all three. Singles are the controls that name each family's own ----
# --- error; the combos are what decide precedence. ---------------------------
# Family Q: unparseable facet.query. Family F: facet.field on an undefined
# field (Solr 400s on undefined; an *unfacetable* field is 200-empty in Solr —
# ratified divergence 2 — so it cannot carry a Solr-side precedence signal;
# see facet_err_query_vs_unfacetable below for that case). Family R:
# facet.range on a string field.
capd facet_err_query_single "$DEBT_CORE/select?q=*:*&rows=0&facet=true&facet.query=views:[bad&wt=json"
capd facet_err_field_single "$DEBT_CORE/select?q=*:*&rows=0&facet=true&facet.field=nosuchfield&wt=json"
capd facet_err_range_single "$DEBT_CORE/select?q=*:*&rows=0&facet=true&facet.range=tag&facet.range.start=0&facet.range.end=10&facet.range.gap=5&wt=json"
capd facet_err_query_field  "$DEBT_CORE/select?q=*:*&rows=0&facet=true&facet.query=views:[bad&facet.field=nosuchfield&wt=json"
capd facet_err_query_range  "$DEBT_CORE/select?q=*:*&rows=0&facet=true&facet.query=views:[bad&facet.range=tag&facet.range.start=0&facet.range.end=10&facet.range.gap=5&wt=json"
capd facet_err_field_range  "$DEBT_CORE/select?q=*:*&rows=0&facet=true&facet.field=nosuchfield&facet.range=tag&facet.range.start=0&facet.range.end=10&facet.range.gap=5&wt=json"
capd facet_err_all_three    "$DEBT_CORE/select?q=*:*&rows=0&facet=true&facet.query=views:[bad&facet.field=nosuchfield&facet.range=tag&facet.range.start=0&facet.range.end=10&facet.range.gap=5&wt=json"
# The #30 shape verbatim: invalid facet.query + *unfacetable* (stored-only)
# facet.field. In Solr the field half is not an error at all, so this pins
# that the query error still surfaces despite the 200-empty field.
capd facet_err_query_vs_unfacetable "$DEBT_CORE/select?q=*:*&rows=0&facet=true&facet.query=views:[bad&facet.field=note&wt=json"

# --- "5" vs "5.0" (#33 item 2): facet.field on pdouble / pfloat --------------
capd facet_field_double_all            "$DEBT_CORE/select?q=*:*&rows=0&facet=true&facet.field=price&wt=json"
capd facet_field_double_subset         "$DEBT_CORE/select?q=id:r1&rows=0&facet=true&facet.field=price&wt=json"
capd facet_field_double_sort_index_all "$DEBT_CORE/select?q=*:*&rows=0&facet=true&facet.field=price&facet.sort=index&wt=json"
capd facet_field_float_all             "$DEBT_CORE/select?q=*:*&rows=0&facet=true&facet.field=rating&wt=json"

# --- millisecond-precision date facet (#33 item 3) ---------------------------
capd facet_field_date_ms_all            "$DEBT_CORE/select?q=*:*&rows=0&facet=true&facet.field=stamp&wt=json"
capd facet_field_date_ms_subset         "$DEBT_CORE/select?q=id:r1&rows=0&facet=true&facet.field=stamp&wt=json"
capd facet_field_date_ms_sort_index_all "$DEBT_CORE/select?q=*:*&rows=0&facet=true&facet.field=stamp&facet.sort=index&wt=json"

# --- unpinned facet semantics (#33 item 4) -----------------------------------
# facet.missing vs facet.limit: does the null bucket survive a limit of 1?
capd facet_missing_with_limit "$DEBT_CORE/select?q=*:*&rows=0&facet=true&facet.field=tag&facet.missing=true&facet.limit=1&wt=json"
# facet.missing vs facet.mincount: null bucket counts 2. mincount=2 keeps
# apple(2) and drops banana(1); mincount=3 drops every real term. Whether the
# null bucket appears at 3 says whether it is subject to mincount at all.
capd facet_missing_with_mincount_two   "$DEBT_CORE/select?q=*:*&rows=0&facet=true&facet.field=tag&facet.missing=true&facet.mincount=2&wt=json"
capd facet_missing_with_mincount_three "$DEBT_CORE/select?q=*:*&rows=0&facet=true&facet.field=tag&facet.missing=true&facet.mincount=3&wt=json"
# facet.range span not divisible by gap, hardend unset: views are 5..45, so
# the last bucket [20,30) counts 25 while [20,22) counts nothing — the counts
# and the echoed `end` both discriminate gap-aligned vs verbatim.
capd facet_range_end_not_gap_aligned "$DEBT_CORE/select?q=*:*&rows=0&facet=true&facet.range=views&facet.range.start=0&facet.range.end=22&facet.range.gap=10&wt=json"
# json.nl variants beyond flat/map, currently accepted and rendered flat.
capd facet_json_nl_arrarr "$DEBT_CORE/select?q=*:*&rows=0&facet=true&facet.field=tag&json.nl=arrarr&wt=json"
capd facet_json_nl_arrmap "$DEBT_CORE/select?q=*:*&rows=0&facet=true&facet.field=tag&json.nl=arrmap&wt=json"
# json.nl=map + facet.missing: a JSON object cannot key on null; what does
# Solr use? (Wayfinder currently renders the empty string.)
capd facet_json_nl_map_missing "$DEBT_CORE/select?q=*:*&rows=0&facet=true&facet.field=tag&facet.missing=true&json.nl=map&wt=json"

release "$DEBT_CONTAINER" "facet-debt core '$DEBT_CORE'"

# --- ranked-relevance scores + fl order (issue #31) -------------------------
# Appended block; nothing above is edited. Runs against the canonical
# `$CONTAINER`/`$CORE` (reused `cap()`), same shared schema/5-doc corpus as the
# tracer-bullet block at the top of this script — so these three rows go in
# `manifest.tsv`, not `manifest-errors.tsv`.
#
# `select_term_scored`/`select_quick_scored`: `fl=id,score` on the two free-text
# queries the differential harness already treats as ranked-relevance entries,
# so `diff_ranked_ids` has a real fixture with per-doc `score` and
# `response.maxScore` to exercise its tolerance path against, not just the
# synthetic unit tests (differential-harness follow-up 1-2).
#
# `select_fl_reversed`: `fl=body,id`, reversed from every other multi-field
# `fl` capture (`select_term`'s `fl=id,body` cannot discriminate order from
# `fl`, since input order and `fl` order coincide there). Pins finding 24's
# "doc key order is input order, not `fl` order" half on a committed fixture
# instead of a live probe only.
cap select_term_scored  'select?q=lazy&df=body&fl=id,score&rows=5&wt=json'
cap select_quick_scored 'select?q=quick&df=body&fl=id,score&rows=5&wt=json'
cap select_fl_reversed  'select?q=*:*&rows=2&fl=body,id&wt=json'

# --- update pipeline: /update envelope, deletes, commit knobs (issue #9) -----
# Appended block; nothing above is edited. Own container on its own port, per
# the `wayfinder-solr-24`/`-32`/`-33` precedent: this block was written while
# issue #8 owned the canonical container (8983), and the top of this script
# rebuilds that container destructively. Same caveat as those blocks: NOT
# runnable standalone -- it uses `$OUT`/`$HERE` from the top of the script, and
# `capu`/`capup` append to `manifest-errors.tsv` unconditionally, so re-running
# just this block duplicates its rows. Run the whole script.
#
# Everything here is a POST (or a deliberately-non-GET / non-reference-core
# request), so every row goes to manifest-errors.tsv, never manifest.tsv.
#
# Deletes MUTATE the corpus, which is exactly what issue #26 warns about: a
# probe must never leave a core unable to reproduce its own fixtures. This
# block's answer is idempotency-by-recreation: the first thing it does on
# every run is delete and recreate the core (schema included — a doc-only
# reset is NOT enough, see the copy-field note below) and reseed the same
# corpus, so the capture sequence always starts from the same state no matter
# what a previous run left behind. The captures below are strictly ordered and
# each comment tracks the corpus state; do not reorder them.
UPDATE9_CONTAINER=wayfinder-solr-9
UPDATE9_SOLR=http://localhost:8989/solr
UPDATE9_CORE=update9
if ! docker ps --format '{{.Names}}' | grep -qx "$UPDATE9_CONTAINER"; then
  docker rm -f "$UPDATE9_CONTAINER" >/dev/null 2>&1 || true
  docker run -d --name "$UPDATE9_CONTAINER" -p 8989:8983 \
    solr:9 solr-precreate "$UPDATE9_CORE" >/dev/null
fi
echo -n "waiting for update9 solr"
for _ in $(seq 60); do
  if curl -sf "$UPDATE9_SOLR/$UPDATE9_CORE/admin/ping?wt=json" >/dev/null 2>&1; then echo " ok"; break; fi
  echo -n "."; sleep 1
done

# Fresh core on EVERY run, not just a doc reset: `add-copy-field` is not
# idempotent the way `add-field` is tolerant — a warm re-run appends a second
# `nick`->`alias` directive, after which every copied value lands twice in the
# single-valued destination and `update_copyfield_single_ok` flips 200 -> 400.
# That is the issue-#26 failure class (a capture run leaving state that cannot
# reproduce its own fixtures), caught live on this block's first warm re-run.
# Deleting the core drops its schema too, so the block always builds both the
# schema and the corpus from scratch.
docker exec "$UPDATE9_CONTAINER" solr delete -c "$UPDATE9_CORE" >/dev/null 2>&1 || true
docker exec "$UPDATE9_CONTAINER" solr create -c "$UPDATE9_CORE" >/dev/null
echo -n "waiting for recreated update9 core"
for _ in $(seq 60); do
  if curl -sf "$UPDATE9_SOLR/$UPDATE9_CORE/admin/ping?wt=json" >/dev/null 2>&1; then echo " ok"; break; fi
  echo -n "."; sleep 1
done

# Schema: `title` is the single-valued docValues string for the
# array-into-single-valued 400; `nick` -> `alias` is the copy-field pair whose
# destination is single-valued (schema-layer follow-ups 1 and 2). `*_dt`
# (dynamic pdate) already exists in the _default configset, which is what the
# dynamic-date round trip uses (schema-layer follow-up 3). The core is always
# freshly created (above), so none of this needs to tolerate leftovers.
curl -s "$UPDATE9_SOLR/$UPDATE9_CORE/schema" -H 'Content-Type: application/json' -d '{
  "add-field": [
    {"name":"body",    "type":"text_en","indexed":true, "stored":true},
    {"name":"category","type":"string", "indexed":true, "stored":true,
     "docValues":true, "multiValued":true},
    {"name":"title",   "type":"string", "indexed":true, "stored":true, "docValues":true},
    {"name":"nick",    "type":"string", "indexed":true, "stored":true, "docValues":true},
    {"name":"alias",   "type":"string", "indexed":true, "stored":true, "docValues":true}
  ],
  "add-copy-field": [
    {"source":"nick", "dest":"alias"}
  ]
}' >/dev/null

# Seed the corpus (the fresh core above is what makes re-runs idempotent;
# NOT captured).
curl -sf "$UPDATE9_SOLR/$UPDATE9_CORE/update?commit=true" -H 'Content-Type: application/json' -d '[
  {"id":"u1","body":"quick brown fox","category":["keep"]},
  {"id":"u2","body":"lazy dog","category":["temp"]},
  {"id":"u3","body":"lazy afternoon","category":["temp"]},
  {"id":"u4","body":"garden path","category":["keep"]},
  {"id":"u5","body":"nothing much here","category":["temp","keep"]}
]' >/dev/null
# Corpus is now exactly u1..u5.

# POST helper, 6-column manifest-errors.tsv contract (the `cap_post` /
# `update_unknown_field_*` precedent: name, status, method, url-after-/solr/,
# body, base URL).
capup() {  # capup <name> <url-after-/solr/> <json-body>
  local name=$1 suffix=$2 body=$3
  want "$name" || return 0
  curl -sg "$UPDATE9_SOLR/$suffix" -H 'Content-Type: application/json' -d "$body" \
    -o "$OUT/$name.json" -w '%{http_code}' > "$OUT/$name.status"
  printf '%s\t%s\t%s\t%s\t%s\t%s\n' \
    "$name" "$(cat "$OUT/$name.status")" POST "$suffix" "$body" "$UPDATE9_SOLR" \
    >> "$MANIFEST_ERRORS"
  rm -f "$OUT/$name.status"
}
# Arbitrary-method helper (GET /update, DELETE /admin/ping, unknown core),
# same 6-column contract, empty body column.
capu() {  # capu <name> <method> <url-after-/solr/>
  local name=$1 method=$2 suffix=$3
  want "$name" || return 0
  curl -sg -X "$method" "$UPDATE9_SOLR/$suffix" \
    -o "$OUT/$name.json" -w '%{http_code}' > "$OUT/$name.status"
  printf '%s\t%s\t%s\t%s\t%s\t%s\n' \
    "$name" "$(cat "$OUT/$name.status")" "$method" "$suffix" "" "$UPDATE9_SOLR" \
    >> "$MANIFEST_ERRORS"
  rm -f "$OUT/$name.status"
}

# --- the /update response envelope (issue #9 scope: "not yet captured") ------
# Does a successful /update echo params? Carry anything besides
# responseHeader? Same envelope for add / delete / commit?
# u6 added WITHOUT commit: _default's autoCommit is maxTime 15s with
# openSearcher=false and autoSoftCommit off, so u6 stays invisible to search
# until a later explicit commit -- the immediately-following select is the
# visibility baseline.
capup update_add_nocommit "$UPDATE9_CORE/update?wt=json" \
  '[{"id":"u6","body":"pending doc","category":["pending"]}]'
capu update_select_uncommitted GET \
  "$UPDATE9_CORE/select?q=id:u6&wt=json"
# Explicit commit=true: u7 visible at once. (This commit also makes u6
# visible; the corpus is now u1..u7.)
capup update_add_commit "$UPDATE9_CORE/update?commit=true&wt=json" \
  '[{"id":"u7","body":"committed doc","category":["keep"]}]'
capu update_select_committed GET \
  "$UPDATE9_CORE/select?q=id:u7&wt=json"

# --- overwrite ---------------------------------------------------------------
# Default overwrite=true: re-adding u7 replaces it (numFound stays 1, body is
# the new one).
capup update_overwrite_default "$UPDATE9_CORE/update?commit=true&wt=json" \
  '[{"id":"u7","body":"replaced body","category":["keep"]}]'
capu update_select_overwritten GET \
  "$UPDATE9_CORE/select?q=id:u7&wt=json"
# overwrite=false: a second live doc with the same uniqueKey.
capup update_overwrite_false "$UPDATE9_CORE/update?commit=true&overwrite=false&wt=json" \
  '[{"id":"u7","body":"duplicate body","category":["dup"]}]'
capu update_select_overwrite_false GET \
  "$UPDATE9_CORE/select?q=id:u7&wt=json"

# --- deletes -----------------------------------------------------------------
# Delete-by-id, object form. u7 exists twice (overwrite=false above): does a
# delete by uniqueKey term remove BOTH? The select decides.
capup update_delete_id_obj "$UPDATE9_CORE/update?commit=true&wt=json" \
  '{"delete":{"id":"u7"}}'
capu update_select_after_delete_id GET \
  "$UPDATE9_CORE/select?q=id:u7&wt=json"
# Delete-by-id, list form: u1 and u4 go. Corpus is now u2,u3,u5,u6.
#
# The corpus-state selects here and below carry an explicit `sort=id asc`,
# deliberately: with `q=*:*` every doc scores identically, and the tie order
# is Lucene/Tantivy internal doc order — a segment-merge-history artifact,
# not a wire contract (the first capture of `update_select_after_mixed`,
# without a sort, pinned merge internals that no other engine — nor even
# another Solr run with different merge timing — is obliged to reproduce).
# What these fixtures exist to pin is WHICH docs survive each mutation;
# the sort makes that assertable deterministically on both sides.
capup update_delete_id_list "$UPDATE9_CORE/update?commit=true&wt=json" \
  '{"delete":["u1","u4"]}'
capu update_select_after_delete_list GET \
  "$UPDATE9_CORE/select?q=*:*&fl=id&rows=20&sort=id+asc&wt=json"
# Delete-by-query on a TEXT field (`body:lazy`), not a string term: pins that
# delete-by-query goes through the same analyzed-query semantics as /select
# ("lazy" matches u2 "lazy dog" and u3 "lazy afternoon" via text_en analysis).
# Corpus is now u5,u6.
capup update_delete_query "$UPDATE9_CORE/update?commit=true&wt=json" \
  '{"delete":{"query":"body:lazy"}}'
capu update_select_after_delete_query GET \
  "$UPDATE9_CORE/select?q=*:*&fl=id&rows=20&sort=id+asc&wt=json"

# --- mixed-command body (capture decides scope, per the issue) ----------------
# add + delete + commit in one body. The delete targets u6; the add is u8.
# If accepted, corpus is now u5,u8.
capup update_mixed_commands "$UPDATE9_CORE/update?wt=json" \
  '{"add":{"doc":{"id":"u8","body":"mixed add","category":["keep"]}},"delete":{"id":"u6"},"commit":{}}'
capu update_select_after_mixed GET \
  "$UPDATE9_CORE/select?q=*:*&fl=id&rows=20&sort=id+asc&wt=json"

# --- commitWithin / softCommit ------------------------------------------------
# commitWithin=500ms, then a settle sleep well past the window before the
# visibility select. Only the settled state is captured -- an immediate select
# would race the window and capture nondeterministic ground truth.
capup update_commitwithin "$UPDATE9_CORE/update?commitWithin=500&wt=json" \
  '[{"id":"u9","body":"commit within doc","category":["keep"]}]'
sleep 3
capu update_select_commitwithin_visible GET \
  "$UPDATE9_CORE/select?q=id:u9&wt=json"
# softCommit=true with no commit param: is the request-end commit soft, and is
# the doc immediately visible?
capup update_softcommit "$UPDATE9_CORE/update?softCommit=true&wt=json" \
  '[{"id":"u10","body":"soft committed doc","category":["keep"]}]'
capu update_select_softcommit_visible GET \
  "$UPDATE9_CORE/select?q=id:u10&wt=json"

# --- GET /update (error-shapes follow-up 2: uncaptured, Wayfinder 400s it) ----
capu update_get GET "$UPDATE9_CORE/update?wt=json"
capu update_get_commit GET "$UPDATE9_CORE/update?commit=true&wt=json"

# --- unknown core on /update and /admin/ping (error-shapes follow-ups 3-4) ----
# err_missing_core only exercised GET /select; these pin whether the HTML 404
# easter egg is endpoint- and method-agnostic.
capup update_unknown_core "nosuchcore/update?commit=true&wt=json" \
  '[{"id":"x","body":"y"}]'
capu ping_unknown_core GET "nosuchcore/admin/ping?wt=json"
capu ping_unknown_core_delete DELETE "nosuchcore/admin/ping?wt=json"

# --- single-valued field given an array (schema-layer follow-up 1) ------------
capup update_single_valued_array "$UPDATE9_CORE/update?commit=true&wt=json" \
  '[{"id":"u11","title":["one","two"]}]'
# --- copy-field into a single-valued destination (schema-layer follow-up 2) ---
# The doc supplies `alias` AND `nick` (which copies into `alias`): two values
# in a single-valued destination. The control (`nick` only) shows the copied
# value alone is fine and what the stored `alias` looks like.
capup update_copyfield_single_valued "$UPDATE9_CORE/update?commit=true&wt=json" \
  '[{"id":"u12","nick":"nn","alias":"aa"}]'
capup update_copyfield_single_ok "$UPDATE9_CORE/update?commit=true&wt=json" \
  '[{"id":"u13","nick":"solo"}]'
capu update_select_copyfield_dest GET \
  "$UPDATE9_CORE/select?q=id:u13&fl=id,nick,alias&wt=json"

# --- dynamic date round trip (schema-layer follow-up 3) ------------------------
# `*_dt` is a dynamic pdate in the _default configset; the select pins both the
# range-query behaviour and the stored rendering of the value.
capup update_dynamic_date "$UPDATE9_CORE/update?commit=true&wt=json" \
  '[{"id":"u14","when_dt":"2021-06-01T12:30:45Z"}]'
capu update_select_dynamic_date GET \
  "$UPDATE9_CORE/select?q=when_dt:%5B2021-01-01T00:00:00Z%20TO%202022-01-01T00:00:00Z%5D&fl=id,when_dt&wt=json"


# --- single-valued edge: a ONE-element array is accepted ----------------------
# Captured after the fact (same run, same corpus state): Solr unwraps a
# one-element array into a single-valued field and stores the scalar, so only
# arrays with MORE than one value are the 400 above.
capup update_single_valued_array_one "$UPDATE9_CORE/update?commit=true&wt=json" \
  '[{"id":"u15","title":["only"]}]'
capu update_select_single_valued_array_one GET \
  "$UPDATE9_CORE/select?q=id:u15&fl=id,title&wt=json"
# Delete-by-id of an id that does not exist: 200, same bare envelope.
capup update_delete_id_missing "$UPDATE9_CORE/update?commit=true&wt=json" \
  '{"delete":{"id":"nosuch"}}'

release "$UPDATE9_CONTAINER" "update-pipeline core '$UPDATE9_CORE'"

# --- pure-wildcard sub-clause (issue #39) ------------------------------------
# `*:* AND lazy` / `lazy OR *:* / `*:* -lazy` panic pre-fix: `*:*` compiles to
# tantivy-query-grammar's `Exists` leaf, which unconditionally requires a field
# once it isn't the whole query string (`set_field(None)` ->
# `.expect("Exist query without a field isn't allowed")`,
# tantivy-query-grammar-0.26.0/src/user_input_ast.rs:51). Wayfinder's
# `parse_query` already special-cases the whole-string `*:*` as `AllQuery`;
# these three probe the sub-clause case that reaches the grammar instead.
#
# Own container on its own port (`wayfinder-solr-39`, 8990), per the
# `wayfinder-solr-24`/`-32`/`-33` precedent: issues #8 (8983, canonical) and #9
# (8989) were running concurrently. Same schema and 5-doc corpus as the
# canonical `content` core at the top of this script, so these are ordinary
# `content`-core GETs and belong in `manifest.tsv`, not `manifest-errors.tsv` —
# NOT run through `cap()` (that targets `$SOLR`/`$CORE`, i.e. the canonical
# container), so appended to `manifest.tsv` by hand below. Not runnable
# standalone: rebuild the container first if it isn't already up.
WILDCARD_CONTAINER=wayfinder-solr-39
WILDCARD_SOLR=http://localhost:8990/solr
WILDCARD_CORE=content
if ! docker ps --format '{{.Names}}' | grep -qx "$WILDCARD_CONTAINER"; then
  docker rm -f "$WILDCARD_CONTAINER" >/dev/null 2>&1 || true
  docker run -d --name "$WILDCARD_CONTAINER" -p 8990:8983 \
    solr:9 solr-precreate "$WILDCARD_CORE" >/dev/null
fi
echo -n "waiting for wildcard-panic solr"
for _ in $(seq 60); do
  if curl -sf "$WILDCARD_SOLR/$WILDCARD_CORE/admin/ping?wt=json" >/dev/null 2>&1; then echo " ok"; break; fi
  echo -n "."; sleep 1
done
curl -s "$WILDCARD_SOLR/$WILDCARD_CORE/schema" -H 'Content-Type: application/json' -d '{
  "add-field": [
    {"name":"body",     "type":"text_en", "indexed":true, "stored":true},
    {"name":"category", "type":"string",  "indexed":true, "stored":true,
     "docValues":true, "multiValued":true}
  ]
}' >/dev/null
curl -sf "$WILDCARD_SOLR/$WILDCARD_CORE/update?commit=true" -H 'Content-Type: application/json' -d '[
    {"id":"doc1","body":"the quick brown fox jumps over the lazy dog","category":["animals","classic"]},
    {"id":"doc2","body":"a lazy afternoon in the garden","category":["garden"]},
    {"id":"doc3","body":"quick thinking saves the day","category":["misc","classic"]},
    {"id":"doc4","body":"dogs and cats living together","category":["animals"]},
    {"id":"doc5","body":"nothing much here at all"}
]' >/dev/null
capw() {  # capw <name> <path-with-query>, against $WILDCARD_SOLR/$WILDCARD_CORE
  local name=$1 path=$2
  want "$name" || return 0
  curl -sg "$WILDCARD_SOLR/$WILDCARD_CORE/$path" -o "$OUT/$name.json" -w '%{http_code}' > "$OUT/$name.status"
  printf '%s\t%s\t%s\n' "$name" "$(cat "$OUT/$name.status")" "$path" >> "$MANIFEST"
  rm -f "$OUT/$name.status"
}
capw select_wildcard_and_term   'select?q=*:*+AND+lazy&df=body&fl=id,body&wt=json'
capw select_wildcard_or_term    'select?q=lazy+OR+*:*&df=body&fl=id,body&wt=json'
capw select_wildcard_minus_term 'select?q=*:*+-lazy&df=body&fl=id,body&wt=json'
release "$WILDCARD_CONTAINER" "wildcard-panic core '$WILDCARD_CORE'"

# --- stats component (issue #5) ---------------------------------------------
# Appended block; nothing above is edited. Own container on its own port
# (`wayfinder-solr-5`, 8992 -- 8983..8990 are all owned by other issues/
# branches per the `wayfinder-solr-24`/`-32`/`-33`/`-39` precedent), own core
# `stats`, own corpus. Not the canonical container, so every row here is a
# `manifest-errors.tsv` 6-column row (own base URL), never `manifest.tsv`,
# exactly like the `-32`/`-33` debt blocks.
#
# Premise check (issue #5's own task spec): the `facets` core's `views`/
# `created` fields (issue #3) have a value on every doc (`r1..r4`), so they
# cannot exercise "missing on some docs" -- stats' `missing` count and its
# min/max/sum/mean/stddev-over-present-values-only requirement need a corpus
# with an actual gap. Hence a dedicated corpus rather than reusing `facets`.
#
# Corpus `st1..st6`: `views` (pint) missing on `st6`, `price` (pdouble)
# missing on `st5` -- two independent gaps so a single `stats.field=views&
# stats.field=price` capture exercises repeatable `stats.field` AND two
# different per-field `missing` counts in one response.
STATS_CONTAINER=wayfinder-solr-5
STATS_SOLR=http://localhost:8992/solr
STATS_CORE=stats
if ! docker ps --format '{{.Names}}' | grep -qx "$STATS_CONTAINER"; then
  docker rm -f "$STATS_CONTAINER" >/dev/null 2>&1 || true
  docker run -d --name "$STATS_CONTAINER" -p 8992:8983 \
    solr:9 solr-precreate "$STATS_CORE" >/dev/null
fi
echo -n "waiting for stats solr"
for _ in $(seq 60); do
  if curl -sf "$STATS_SOLR/$STATS_CORE/admin/ping?wt=json" >/dev/null 2>&1; then echo " ok"; break; fi
  echo -n "."; sleep 1
done
curl -s "$STATS_SOLR/$STATS_CORE/schema" -H 'Content-Type: application/json' -d '{
  "add-field": [
    {"name":"views", "type":"pint",    "indexed":true, "stored":true, "docValues":true},
    {"name":"price", "type":"pdouble", "indexed":true, "stored":true, "docValues":true}
  ]
}' >/dev/null
curl -sf "$STATS_SOLR/$STATS_CORE/update?commit=true" -H 'Content-Type: application/json' -d '[
  {"id":"st1","views":10,"price":1.5},
  {"id":"st2","views":20,"price":2.5},
  {"id":"st3","views":30,"price":3.5},
  {"id":"st4","views":40,"price":4.5},
  {"id":"st5","views":50},
  {"id":"st6","price":5.5}
]' >/dev/null

# Same 6-column manifest-errors.tsv contract as `capd`/`capw` above.
caps() {  # caps <name> <url-after-/solr/>
  local name=$1 suffix=$2
  want "$name" || return 0
  curl -sg "$STATS_SOLR/$suffix" -o "$OUT/$name.json" -w '%{http_code}' > "$OUT/$name.status"
  printf '%s\t%s\t%s\t%s\t%s\t%s\n' \
    "$name" "$(cat "$OUT/$name.status")" GET "$suffix" "" "$STATS_SOLR" \
    >> "$MANIFEST_ERRORS"
  rm -f "$OUT/$name.status"
}

# Normal case: whole corpus, one stats.field with a real gap (st6 has no
# `views`), so min/max/sum/mean/stddev must come from the 5 docs that have a
# value while `missing` reports the 1 that does not.
caps stats_views "$STATS_CORE/select?q=*:*&rows=0&stats=true&stats.field=views&wt=json"
# Repeatable stats.field: two fields in one request, each with its OWN
# missing doc (`views` misses st6, `price` misses st5) -- proves `missing` is
# computed per field, not shared.
caps stats_multi_fields "$STATS_CORE/select?q=*:*&rows=0&stats=true&stats.field=views&stats.field=price&wt=json"
# Zero matching docs: what does the stats block look like when q matches
# nothing at all (not just a field-level gap)?
caps stats_zero "$STATS_CORE/select?q=id:nosuchdoc&rows=0&stats=true&stats.field=views&wt=json"
# Zero matching docs via fq narrowing rather than q itself, per the task
# spec's "or an fq narrows to nothing" alternative -- pins that both paths to
# a zero hit set produce the same stats shape.
caps stats_zero_fq "$STATS_CORE/select?q=*:*&fq=id:nosuchdoc&rows=0&stats=true&stats.field=views&wt=json"

release "$STATS_CONTAINER" "stats core '$STATS_CORE'"

# --- highlighting (issue #4, PRD Highlighting row: hl, hl.fl, hl.snippets, ---
# hl.fragsize, hl.simple.pre/post, Tantivy SnippetGenerator) ------------------
# Own container, own port (`wayfinder-solr-4`, 8991), per the
# `wayfinder-solr-24`/`-32`/`-33`/`-39` precedent: several issue containers
# (8983-8990) are already running concurrently and none may be reused or
# stopped. Same schema and 5-doc "quick brown fox" corpus as the canonical
# `content` core at the top of this script, so these are ordinary
# `content`-core GETs and belong in `manifest.tsv` (via `caph()` below), not
# `manifest-errors.tsv`. Not runnable standalone: this block builds its own
# container from scratch every run rather than reusing state, since
# highlighting has no captures yet to protect.
HL_CONTAINER=wayfinder-solr-4
HL_SOLR=http://localhost:8991/solr
HL_CORE=content
docker rm -f "$HL_CONTAINER" >/dev/null 2>&1 || true
docker run -d --name "$HL_CONTAINER" -p 8991:8983 \
  solr:9 solr-precreate "$HL_CORE" >/dev/null
echo -n "waiting for highlighting solr"
for _ in $(seq 60); do
  if curl -sf "$HL_SOLR/$HL_CORE/admin/ping?wt=json" >/dev/null 2>&1; then echo " ok"; break; fi
  echo -n "."; sleep 1
done
curl -s "$HL_SOLR/$HL_CORE/schema" -H 'Content-Type: application/json' -d '{
  "add-field": [
    {"name":"body",     "type":"text_en", "indexed":true, "stored":true},
    {"name":"category", "type":"string",  "indexed":true, "stored":true,
     "docValues":true, "multiValued":true}
  ]
}' >/dev/null
curl -sf "$HL_SOLR/$HL_CORE/update?commit=true" -H 'Content-Type: application/json' -d '[
    {"id":"doc1","body":"the quick brown fox jumps over the lazy dog","category":["animals","classic"]},
    {"id":"doc2","body":"a lazy afternoon in the garden","category":["garden"]},
    {"id":"doc3","body":"quick thinking saves the day","category":["misc","classic"]},
    {"id":"doc4","body":"dogs and cats living together","category":["animals"]},
    {"id":"doc5","body":"nothing much here at all"}
]' >/dev/null
caph() {  # caph <name> <path-with-query>, against $HL_SOLR/$HL_CORE
  local name=$1 path=$2
  want "$name" || return 0
  curl -sg "$HL_SOLR/$HL_CORE/$path" -o "$OUT/$name.json" -w '%{http_code}' > "$OUT/$name.status"
  printf '%s\t%s\t%s\n' "$name" "$(cat "$OUT/$name.status")" "$path" >> "$MANIFEST"
  rm -f "$OUT/$name.status"
}

# basic single-field highlight, matches doc1 ("lazy dog") and doc2 ("lazy afternoon")
caph hl_basic            'select?q=lazy&df=body&hl=true&hl.fl=body&wt=json'

# hl.snippets: no doc repeats a non-stopword term, so this pins the shape
# Solr actually returns for hl.snippets>1 against a single-occurrence field,
# not an assumed multi-snippet array.
caph hl_snippets_two     'select?q=quick&df=body&hl=true&hl.fl=body&hl.snippets=2&wt=json'

# custom pre/post markers instead of the <em>/</em> default
caph hl_custom_markers   'select?q=lazy&df=body&hl=true&hl.fl=body&hl.simple.pre=%3Cb%3E&hl.simple.post=%3C%2Fb%3E&wt=json'

# small hl.fragsize under the default (unified) hl.method: surprising rather
# than a truncation -- captured as-is rather than assumed. The unified
# highlighter's break iterator never cuts inside what it considers a single
# sentence, so a short, punctuation-free field comes back whole regardless of
# how small hl.fragsize is (verified down to hl.fragsize=1, not just 18).
caph hl_fragsize_small   'select?q=quick&df=body&hl=true&hl.fl=body&hl.fragsize=18&wt=json'

# hl.method=original (the classic Highlighter, not the hl.method=unified
# default) DOES truncate to hl.fragsize -- this is the shape Tantivy's
# SnippetGenerator's own char-budget truncation actually resembles, so it is
# the fixture the fragsize test derives its truncation assertion from.
caph hl_fragsize_truncated 'select?q=quick&df=body&hl=true&hl.fl=body&hl.method=original&hl.fragsize=10&wt=json'

# A doc that matches via `fq` (a non-highlighted, non-scoring field) but
# whose `body` has no term overlap with the query at all -- the crux capture:
# what shape does Solr give that doc's entry under `highlighting`? Uses
# q=*:*&fq=category:animals rather than q=category:animals directly: the
# latter was tried first and produced doc4-before-doc1 in Wayfinder against
# doc1-before-doc4 in Solr -- a real but unrelated BM25/norm ranking
# divergence on a bare category-field term query, not a highlighting
# question. q=*:*+fq gives every matching doc an identical score, so the
# ascending-doc-order tie-break (finding 19) is deterministic on both
# engines and the fixture isolates the highlighting fact it exists to pin.
caph hl_no_field_match   'select?q=*:*&fq=category:animals&hl=true&hl.fl=body&wt=json'

# hl.fl with multiple fields, comma-separated
caph hl_multi_field_comma 'select?q=lazy&df=body&hl=true&hl.fl=body,category&wt=json'
# hl.fl with multiple fields, space-separated (URL-encoded space)
caph hl_multi_field_space 'select?q=lazy&df=body&hl=true&hl.fl=body%20category&wt=json'

# hl=true with no hl.fl at all -- capture Solr's default rather than guessing
caph hl_default_fl        'select?q=lazy&df=body&hl=true&wt=json'

release "$HL_CONTAINER" "highlighting core '$HL_CORE'"
# --- query types beyond the stock parser (issue #8) ---------------------------
# Appended block; nothing above is edited. Findings 56-59 (docs/solr-ref-findings.md).
#
# Two halves:
#   1. Content-core captures: plain core-relative GETs against the untouched
#      5-doc corpus -> manifest.tsv, so the differential harness replays them
#      for free. Covers fuzzy / wildcard / regex / string ranges / boosts and
#      one syntax-error case per type (issue #11's error shapes).
#   2. Numeric/date ranges need numeric/date fields the content corpus lacks.
#      Read-only GETs against the existing `facets` core (built by the issue #3
#      block above, SAME container/port 8983) -> manifest-errors.tsv, per the
#      "core-relative GET only" manifest.tsv contract. The facets schema and
#      corpus are NOT touched: every row here is a GET, so no pre-existing
#      facet fixture can move.
# Not runnable standalone ($OUT/$HERE, unconditional manifest appends): run the
# whole script.

# -- fuzzy: default edit distance, explicit distances, analysis of the fuzzy --
# -- term. `animols` is distance 1 from the indexed `animals`, `animblz` is ---
# -- distance 2 -- the pair discriminates `~` (default), `~1`, `~2`. ----------
cap fuzzy_default_dist1   'select?q=category:animols~&wt=json'
cap fuzzy_default_dist2   'select?q=category:animblz~&wt=json'
cap fuzzy_dist1_hit       'select?q=category:animols~1&wt=json'
cap fuzzy_dist1_miss      'select?q=category:animblz~1&wt=json'
cap fuzzy_dist2           'select?q=category:animblz~2&wt=json'
cap fuzzy_dist0_exact     'select?q=category:animals~0&wt=json'
# text_en indexes `lazy` stemmed to `lazi`. If the fuzzy term were stemmed too,
# `lazy~0` would hit (lazi==lazi); if not, `~0` misses and `~1` hits on the
# 1-edit lazy->lazi distance. This pair pins whether fuzzy terms are analyzed.
cap fuzzy_analyzed_dist0  'select?q=body:lazy~0&wt=json'
cap fuzzy_analyzed_dist1  'select?q=body:lazy~1&wt=json'
# Is the fuzzy term lowercased (multiterm analysis)?
cap fuzzy_analyzed_case   'select?q=body:LAZY~1&wt=json'
# Error shapes: distance above Lucene's max (2), and a fractional distance.
cap err_fuzzy_dist3       'select?q=category:animals~3&wt=json'
cap err_fuzzy_fractional  'select?q=category:animals~0.8&wt=json'

# -- wildcard / prefix: trailing *, ?, leading *, infix *, case, and analysis -
cap wildcard_prefix       'select?q=category:anim*&wt=json'
cap wildcard_qmark        'select?q=category:anima?s&wt=json'
cap wildcard_leading      'select?q=category:*mals&wt=json'
cap wildcard_infix        'select?q=category:an*ls&wt=json'
# Stemming discriminator: index term is `lazi`. `laz*` matches it; `lazy*`
# only matches if the wildcard term were stemmed (lazy->lazi) -- expect miss.
cap wildcard_analyzed_hit  'select?q=body:laz*&wt=json'
cap wildcard_analyzed_stem 'select?q=body:lazy*&wt=json'
cap wildcard_analyzed_case 'select?q=body:LAZ*&wt=json'
# Bare wildcard term against df, and the field-exists idiom `field:*`.
cap wildcard_bare_df      'select?q=laz*&df=body&wt=json'
cap wildcard_field_exists 'select?q=category:*&wt=json'

# -- regex: anchoring (substring vs whole-term), metachars, case, analysis ----
cap regex_full            'select?q=category:/animals/&wt=json'
cap regex_substring       'select?q=category:/anim/&wt=json'
cap regex_dotstar         'select?q=category:/anim.*/&wt=json'
cap regex_charclass       'select?q=category:/anim[a-z]ls/&wt=json'
cap regex_uppercase       'select?q=category:/ANIMALS/&wt=json'
# Against the stemmed text field: /laz./ matches the indexed `lazi`.
cap regex_analyzed        'select?q=body:/laz./&wt=json'
cap err_regex_bad_class   'select?q=category:/anim[/&wt=json'
cap err_regex_unclosed    'select?q=category:/animals&wt=json'

# -- ranges on a string field (numeric/date below, facets core) ---------------
# category terms: animals, classic, garden, misc.
cap range_str_incl        'select?q=category:[animals+TO+garden]&wt=json'
cap range_str_excl        'select?q=category:{animals+TO+garden}&wt=json'
cap range_str_half_open   'select?q=category:[animals+TO+garden}&wt=json'
cap range_str_star_upper  'select?q=category:[garden+TO+*]&wt=json'
cap range_str_star_lower  'select?q=category:[*+TO+classic]&wt=json'
cap range_str_star_both   'select?q=category:[*+TO+*]&wt=json'
cap range_str_reversed    'select?q=category:[garden+TO+animals]&wt=json'
cap err_range_unclosed_q  'select?q=category:[animals+TO&wt=json'
cap err_range_lowercase_to 'select?q=category:[animals+to+garden]&wt=json'

# -- boosts in the query string ------------------------------------------------
# Baseline vs boosted pair: with q=quick garden on body, `garden` (1 doc) has
# the higher idf, so doc2 leads the baseline; boosting quick^10 must pull the
# two quick docs (doc1, doc3) above it. Ordering, not scores, is the contract
# -- BM25 float values are engine-specific.
cap boost_baseline        'select?q=quick+garden&df=body&wt=json'
cap boost_term            'select?q=quick^10+garden&df=body&wt=json'
cap boost_fielded_term    'select?q=body:quick^10+body:garden&wt=json'
cap boost_float           'select?q=body:quick^2.5+body:garden&wt=json'
cap boost_phrase          'select?q=%22lazy+dog%22^2&df=body&wt=json'
cap boost_fuzzy_combo     'select?q=category:animols~1^3&wt=json'
cap err_boost_bad         'select?q=body:quick^bad&wt=json'

# -- quoted phrases are not field queries --------------------------------------
# The discriminator for Wayfinder's dynamic-field rewrite scan (schema-layer
# follow-up 5): a colon inside a quoted phrase must NOT be parsed as
# field:value. Control first: the same text unquoted IS a field query (2 docs).
cap select_q_field_term   'select?q=category:animals&wt=json'
cap phrase_with_colon     'select?q=%22category:animals%22&df=body&wt=json'

# -- numeric/date ranges: facets core (same container), manifest-errors.tsv ----
# views: r1=5 r2=15 r3=25 r4=35; created: r1=2020-01-02 r2=r3=2020-01-03,
# r4=2020-01-05. Same 6-column contract as capf/capk/caps above.
capq8() {  # capq8 <name> <path-after-core>
  local name=$1 suffix=$2
  want "$name" || return 0
  curl -sg "$SOLR/facets/$suffix" -o "$OUT/$name.json" -w '%{http_code}' > "$OUT/$name.status"
  printf '%s\t%s\t%s\t%s\t%s\t%s\n' \
    "$name" "$(cat "$OUT/$name.status")" GET "facets/$suffix" "" "$SOLR" \
    >> "$MANIFEST_ERRORS"
  rm -f "$OUT/$name.status"
}

capq8 qrange_int_incl            'select?q=views:[10+TO+30]&wt=json'
capq8 qrange_int_incl_endpoints  'select?q=views:[5+TO+35]&wt=json'
capq8 qrange_int_excl            'select?q=views:{5+TO+35}&wt=json'
capq8 qrange_int_half_open       'select?q=views:[5+TO+35}&wt=json'
capq8 qrange_int_star_upper      'select?q=views:[25+TO+*]&wt=json'
# Endpoint typing on a pint field: a float endpoint, an alphabetic endpoint,
# a leading-zero literal -- do they coerce, truncate, or 400?
capq8 qrange_int_float_endpoint  'select?q=views:[10.5+TO+30]&wt=json'
capq8 qrange_int_alpha_endpoint  'select?q=views:[a+TO+b]&wt=json'
capq8 qterm_int_leading_zero     'select?q=views:015&wt=json'
# Wildcard / fuzzy against a Points-based field: is there anything to match?
capq8 qwild_int                  'select?q=views:1*&wt=json'
capq8 qfuzzy_int                 'select?q=views:15~1&wt=json'
# Date ranges, inclusive and exclusive.
capq8 qrange_date_incl 'select?q=created:[2020-01-02T00:00:00Z+TO+2020-01-03T00:00:00Z]&wt=json'
capq8 qrange_date_excl 'select?q=created:{2020-01-02T00:00:00Z+TO+2020-01-05T00:00:00Z}&wt=json'

echo "issue #8 query-type captures done (content core -> manifest.tsv, facets core -> manifest-errors.tsv)"

# -- issue #8, review round 1: discriminating cases the first pass left unpinned
# (same block ownership; appended so re-runs stay mechanical).
#
# Transposition: `animasl` swaps the last two chars of `animals` — Damerau
# distance 1, plain-Levenshtein distance 2. `~1` hitting is what pins Lucene's
# transpositions=true default; `~2` is the both-algorithms control.
cap fuzzy_transposition_dist1   'select?q=category:animasl~1&wt=json'
cap fuzzy_transposition_control 'select?q=category:animasl~2&wt=json'
# Compound queries containing a wildcard/fuzzy clause: these must behave as
# boolean composition (Solr), never collapse into one glob or silently drop
# the suffix.
cap compound_wildcard_or   'select?q=category:animals+OR+body:laz*&wt=json'
cap compound_wildcard_and  'select?q=body:laz*+AND+category:animals&wt=json'
cap compound_fuzzy_or      'select?q=category:animols~1+OR+body:garden&wt=json'
cap grouped_wildcard       'select?q=(body:laz*)&wt=json'
# field-exists on a text field with no docValues (`body`): Solr answers from
# the postings, so this must be a 200 with every doc that has a body.
cap exists_non_docvalues   'select?q=body:*&wt=json'

# -- issue #8, review round 2: all-negative / mixed-negative queries ----------
# Solr answers a purely negative query as the complement of its matches (the
# implicit *:* MUST clause); Lucene alone matches nothing. These pin which.
cap negative_only          'select?q=-lazy&df=body&wt=json'
cap negative_not_keyword   'select?q=NOT+lazy&df=body&wt=json'
cap negative_two_clauses   'select?q=-lazy+-dog&df=body&wt=json'
cap negative_and_not       'select?q=lazy+AND+NOT+dog&df=body&wt=json'
cap negative_fielded_and_not 'select?q=category:animals+AND+NOT+body:garden&wt=json'
# --- MoreLikeThis (issue #6) --------------------------------------------------
# `/mlt` needs term statistics that mean something, which the 5-doc
# tracer-bullet corpus cannot give: with 5 docs almost every term is either
# unique to one doc or shared by all of them, so `mlt.mindf`/`mlt.maxdf`
# tuning has nothing to bite on. Own container on its own port
# (`wayfinder-solr-6`, 8993, per the `wayfinder-solr-39`/8990 precedent above),
# own 20-doc corpus with real overlapping/disjoint vocabulary across four
# topic clusters (cooking, gardening, astronomy, outdoors) plus two
# deliberately unrelated docs. Same schema shape as the canonical `content`
# core (`body` text_en, `category` string/fast/multiValued), so this is an
# ordinary `content`-core GET set — belongs in `manifest.tsv`, not
# `manifest-errors.tsv`, via its own `capm()` mirroring `capw()`.
MLT_CONTAINER=wayfinder-solr-6
MLT_SOLR=http://localhost:8993/solr
MLT_CORE=content
if ! docker ps --format '{{.Names}}' | grep -qx "$MLT_CONTAINER"; then
  docker rm -f "$MLT_CONTAINER" >/dev/null 2>&1 || true
  docker run -d --name "$MLT_CONTAINER" -p 8993:8983 \
    solr:9 solr-precreate "$MLT_CORE" >/dev/null
fi
echo -n "waiting for mlt solr"
for _ in $(seq 60); do
  if curl -sf "$MLT_SOLR/$MLT_CORE/admin/ping?wt=json" >/dev/null 2>&1; then echo " ok"; break; fi
  echo -n "."; sleep 1
done
curl -s "$MLT_SOLR/$MLT_CORE/schema" -H 'Content-Type: application/json' -d '{
  "add-field": [
    {"name":"body",     "type":"text_en", "indexed":true, "stored":true},
    {"name":"category", "type":"string",  "indexed":true, "stored":true,
     "docValues":true, "multiValued":true}
  ]
}' >/dev/null
# The `_default` configset's solrconfig.xml (Solr 7+, managed-schema-based)
# does NOT register a `/mlt` request handler the way the classic example
# configs used to -- a bare `solr-precreate` core 404s every /mlt request
# (findings: issue #6). Add it via the Config API; tolerant of "already
# exists" on a warm re-run the same way the schema add-field calls above are.
curl -s "$MLT_SOLR/$MLT_CORE/config" -H 'Content-Type: application/json' -d '{
  "add-requesthandler": {
    "name": "/mlt",
    "class": "solr.MoreLikeThisHandler"
  }
}' >/dev/null
# Four topic clusters (cooking, gardening, astronomy, outdoors) with shared
# vocabulary within a cluster and little across clusters, plus two
# deliberately unrelated docs (mlt19, mlt20) for the no-interesting-terms
# case. mlt1/mlt2 are near-duplicates (pasta/tomatoes/basil) so a baseline
# `/mlt?q=id:mlt1` has an obvious top match; mlt11-mlt15 (astronomy) share
# "night sky"/"stars"/"distant" densely, which is what the mintf/mindf/maxdf
# and minwl/maxwl/maxqt captures tune against.
curl -sf "$MLT_SOLR/$MLT_CORE/update?commit=true" -H 'Content-Type: application/json' -d '[
    {"id":"mlt1", "body":"the chef prepared a delicious pasta dish with fresh tomatoes and basil","category":["cooking","italian"]},
    {"id":"mlt2", "body":"fresh basil and ripe tomatoes make a wonderful pasta sauce","category":["cooking","italian"]},
    {"id":"mlt3", "body":"grilling chicken with garlic and rosemary is a classic dinner","category":["cooking","grilling"]},
    {"id":"mlt4", "body":"roasted vegetables with olive oil and garlic taste amazing","category":["cooking","vegetarian"]},
    {"id":"mlt5", "body":"baking bread requires yeast flour water and patience","category":["cooking","baking"]},
    {"id":"mlt6", "body":"planting tomatoes and basil in the garden this spring","category":["gardening"]},
    {"id":"mlt7", "body":"the garden needs watering every morning during summer heat","category":["gardening"]},
    {"id":"mlt8", "body":"pruning rose bushes keeps the garden looking tidy","category":["gardening"]},
    {"id":"mlt9", "body":"composting kitchen scraps enriches garden soil naturally","category":["gardening"]},
    {"id":"mlt10","body":"growing herbs like basil and rosemary indoors year round","category":["gardening","cooking"]},
    {"id":"mlt11","body":"astronomers observed a bright comet streaking across the night sky","category":["astronomy"]},
    {"id":"mlt12","body":"the telescope revealed distant galaxies and bright stars","category":["astronomy"]},
    {"id":"mlt13","body":"a lunar eclipse darkened the night sky for hours","category":["astronomy"]},
    {"id":"mlt14","body":"scientists study the orbit of planets around distant stars","category":["astronomy"]},
    {"id":"mlt15","body":"the night sky was clear enough to see the milky way","category":["astronomy"]},
    {"id":"mlt16","body":"hiking through the mountains offers stunning views of the valley","category":["outdoors"]},
    {"id":"mlt17","body":"camping near the lake was peaceful and quiet at night","category":["outdoors"]},
    {"id":"mlt18","body":"the river flows quietly through the quiet forest valley","category":["outdoors"]},
    {"id":"mlt19","body":"a short trip to buy office supplies and paper clips","category":["misc"]},
    {"id":"mlt20","body":"nothing here relates to any other document in this corpus","category":["misc"]}
]' >/dev/null
capm() {  # capm <name> <path-with-query>, against $MLT_SOLR/$MLT_CORE
  local name=$1 path=$2
  want "$name" || return 0
  curl -sg "$MLT_SOLR/$MLT_CORE/$path" -o "$OUT/$name.json" -w '%{http_code}' > "$OUT/$name.status"
  printf '%s\t%s\t%s\n' "$name" "$(cat "$OUT/$name.status")" "$path" >> "$MANIFEST"
  rm -f "$OUT/$name.status"
}
# baseline: mlt1's nearest neighbour should be mlt2 (near-duplicate vocabulary)
capm mlt_baseline              'mlt?q=id:mlt1&mlt.fl=body,category&wt=json'
# mlt.fl restricted to one field
capm mlt_fl_restricted         'mlt?q=id:mlt1&mlt.fl=body&wt=json'
# mlt.mintf / mlt.mindf / mlt.maxdf tuning, against the denser astronomy cluster
capm mlt_mintf_mindf_maxdf     'mlt?q=id:mlt11&mlt.fl=body&mlt.mintf=1&mlt.mindf=1&mlt.maxdf=10&wt=json'
# mlt.minwl / mlt.maxwl (word-length gate on interesting terms). Stacked on
# the same mintf=1/mindf=1 loosening as the tuning capture above -- the
# handler's real defaults (mintf=2, mindf=5) are already too strict for a
# 20-doc corpus, so every default-threshold capture below is a genuine 0-hit
# result (see mlt_baseline/mlt_fl_restricted); this and the next three
# captures deliberately loosen mintf/mindf first so the *other* param's
# narrowing effect is visible against a non-empty baseline.
capm mlt_minwl_maxwl           'mlt?q=id:mlt11&mlt.fl=body&mlt.mintf=1&mlt.mindf=1&mlt.minwl=6&mlt.maxwl=10&wt=json'
# mlt.maxqt caps how many interesting terms feed the generated query
capm mlt_maxqt                 'mlt?q=id:mlt11&mlt.fl=body&mlt.mintf=1&mlt.mindf=1&mlt.maxqt=2&wt=json'
# mlt.boost turns on per-term IDF-ish boosting
capm mlt_boost                 'mlt?q=id:mlt1&mlt.fl=body&mlt.mintf=1&mlt.mindf=1&mlt.boost=true&wt=json'
# standard fl/rows/start applied to the MLT result set
capm mlt_fl_rows_start         'mlt?q=id:mlt11&mlt.fl=body&mlt.mintf=1&mlt.mindf=1&fl=id,score&rows=2&start=1&wt=json'
# mlt.interestingTerms=details, to capture the real key/shape it adds
capm mlt_interesting_terms_details 'mlt?q=id:mlt1&mlt.fl=body&mlt.interestingTerms=details&wt=json'
# a doc with no meaningfully-shared vocabulary: genuinely-empty-or-degenerate
# result shape, captured rather than guessed
capm mlt_no_interesting_terms  'mlt?q=id:mlt20&mlt.fl=body&wt=json'
# a source doc that does not exist at all
capm mlt_nonexistent_doc       'mlt?q=id:nosuchdoc&mlt.fl=body&wt=json'
release "$MLT_CONTAINER" "mlt core '$MLT_CORE'"

# --- edismax query parser (issue #7) ----------------------------------------
# `defType=edismax` needs `qf`/`pf` to reward two *different analyzed fields*
# differently, and `tie`/`bq`/`boost` to have a visible, per-doc-distinct
# scoring effect. The 5-doc tracer-bullet corpus has only one text field
# (`body`) and an unanalyzed `category`, so it cannot exercise any of that —
# same rationale as the MLT block above (own container, own corpus). Own
# container on its own port (`wayfinder-solr-7`, 8994, continuing the
# per-issue port precedent), own 10-doc corpus over two text_en fields
# (`title`, `body`), built purpose-first for each edismax knob:
#   - eA/eB: "rocket launch success" split across title-only vs body-only,
#     so `qf=title^N body` and `qf=title body^N` visibly swap which of the
#     two ranks first (finding, see docs/solr-ref-findings.md).
#   - eC/eD: "rocket" present in *both* fields for eC (so `tie` has
#     something to blend) but only in eD's title (so `tie` cannot move eD at
#     all) — isolates tie's effect to exactly one side of the pair.
#   - eA-eD together: `bq=title:mission^5` matches only eC/eD (both have
#     "mission" in the title) and `boost=2` is a pure multiplier — both
#     visible against the same four-doc, `q=rocket` baseline.
#   - pA/pB: same two words ("quick", "fox"), adjacent in pA's body,
#     separated in pB's — `pf=body` rewards pA only; without it the two tie
#     exactly (they carry identical term frequencies otherwise).
#   - mmA-D: "alpha beta gamma" split 3/2/1/0 words per doc, so a given `mm`
#     spec's required-match count reads directly off which doc drops out.
# Core named `content`, per the same convention the MLT block documents:
# Wayfinder's own test core name is independent of the Solr core the
# fixtures were captured from.
EDISMAX_CONTAINER=wayfinder-solr-7
EDISMAX_SOLR=http://localhost:8994/solr
EDISMAX_CORE=content
if ! docker ps --format '{{.Names}}' | grep -qx "$EDISMAX_CONTAINER"; then
  docker rm -f "$EDISMAX_CONTAINER" >/dev/null 2>&1 || true
  docker run -d --name "$EDISMAX_CONTAINER" -p 8994:8983 \
    solr:9 solr-precreate "$EDISMAX_CORE" >/dev/null
fi
echo -n "waiting for edismax solr"
for _ in $(seq 60); do
  if curl -sf "$EDISMAX_SOLR/$EDISMAX_CORE/admin/ping?wt=json" >/dev/null 2>&1; then echo " ok"; break; fi
  echo -n "."; sleep 1
done
curl -s "$EDISMAX_SOLR/$EDISMAX_CORE/schema" -H 'Content-Type: application/json' -d '{
  "add-field": [
    {"name":"title", "type":"text_en", "indexed":true, "stored":true},
    {"name":"body",  "type":"text_en", "indexed":true, "stored":true}
  ]
}' >/dev/null
curl -sf "$EDISMAX_SOLR/$EDISMAX_CORE/update?commit=true" -H 'Content-Type: application/json' -d '[
    {"id":"eA",  "title":"rocket launch success",              "body":"filler unrelated text about weather"},
    {"id":"eB",  "title":"filler unrelated text about weather", "body":"rocket launch success"},
    {"id":"eC",  "title":"rocket mission",                      "body":"the rocket soared past the rocket pad toward the rocket"},
    {"id":"eD",  "title":"rocket rocket rocket mission control", "body":"launch complete"},
    {"id":"pA",  "title":"phrase doc a",                         "body":"a quick fox ran away"},
    {"id":"pB",  "title":"phrase doc b",                         "body":"a fox that is quick ran away"},
    {"id":"mmA", "title":"mm doc a",                             "body":"alpha beta gamma"},
    {"id":"mmB", "title":"mm doc b",                             "body":"alpha beta"},
    {"id":"mmC", "title":"mm doc c",                             "body":"alpha"},
    {"id":"mmD", "title":"mm doc d",                             "body":"nothing relevant here at all"}
]' >/dev/null
cape() {  # cape <name> <path-with-query>, against $EDISMAX_SOLR/$EDISMAX_CORE
  local name=$1 path=$2
  want "$name" || return 0
  curl -sg "$EDISMAX_SOLR/$EDISMAX_CORE/$path" -o "$OUT/$name.json" -w '%{http_code}' > "$OUT/$name.status"
  printf '%s\t%s\t%s\n' "$name" "$(cat "$OUT/$name.status")" "$path" >> "$MANIFEST"
  rm -f "$OUT/$name.status"
}
# happy path / envelope shape
cape edismax_basic              'select?q=rocket&defType=edismax&qf=title+body&fl=id&wt=json'
# qf per-field boost changing which of eA/eB (title-only vs body-only match)
# ranks first — three captures over the same query, boost moved each way
cape edismax_qf_equal           'select?q=rocket+launch+success&defType=edismax&qf=title+body&fl=id&wt=json'
cape edismax_qf_boost_title     'select?q=rocket+launch+success&defType=edismax&qf=title^10+body&fl=id&wt=json'
cape edismax_qf_boost_body      'select?q=rocket+launch+success&defType=edismax&qf=title+body^10&fl=id&wt=json'
# pf phrase boost: pA (adjacent "quick fox") vs pB (same two words, not
# adjacent) tie exactly without pf, pA pulls ahead with it
cape edismax_pf_off             'select?q=quick+fox&defType=edismax&qf=body&fl=id,score&fq=id:(pA+OR+pB)&wt=json'
cape edismax_pf_on              'select?q=quick+fox&defType=edismax&qf=body&pf=body&fl=id,score&fq=id:(pA+OR+pB)&wt=json'
# tie dis-max tie-break: eC matches "rocket" in both fields (tie has
# something to blend), eD only in its title (tie cannot move it)
cape edismax_tie_0              'select?q=rocket&defType=edismax&qf=title+body&tie=0&fl=id,score&fq=id:(eC+OR+eD)&wt=json'
cape edismax_tie_1              'select?q=rocket&defType=edismax&qf=title+body&tie=1&fl=id,score&fq=id:(eC+OR+eD)&wt=json'
# boost (multiplicative) / bq (additive) against the same four-doc baseline
cape edismax_score_baseline     'select?q=rocket&defType=edismax&qf=title+body&fl=id,score&fq=id:(eA+OR+eB+OR+eC+OR+eD)&wt=json'
cape edismax_boost_multiplicative 'select?q=rocket&defType=edismax&qf=title+body&boost=2&fl=id,score&fq=id:(eA+OR+eB+OR+eC+OR+eD)&wt=json'
cape edismax_bq_additive        'select?q=rocket&defType=edismax&qf=title+body&bq=title:mission^5&fl=id,score&fq=id:(eA+OR+eB+OR+eC+OR+eD)&wt=json'
# in-query term boost (issue #109): `rocket^5` inside `q` itself, distinct
# from `boost=`/`bq=`. Reverified for issue #51 on 2026-07-30 in a clean,
# isolated `solr:9` container using this block's exact schema/corpus and two
# same-container requests (`q=rocket`, then `q=rocket^5`); the committed
# term-boost response is exactly 5x that baseline per eA-eD. The temporary
# captures were kept outside the repository, so this script was not rerun.
cape edismax_term_boost         'select?q=rocket^5&defType=edismax&qf=title+body&fl=id,score&fq=id:(eA+OR+eB+OR+eC+OR+eD)&wt=json'
# qf naming one undefined field among otherwise-valid ones (issue #111):
# confirmed against real Solr (one-off container, same schema as this block
# -- not re-run through this script) that it 400s on the undefined name
# alone, even though `title` in the same `qf` is valid. Deliberately not a
# `cape` call / manifest.tsv row: it's an error envelope, and the generic
# hermetic sweep (`hermetic_edismax_manifest_entries_match_committed_fixtures`)
# compares `error.msg`/`error.metadata` verbatim, which would always fail
# (Solr's Java exception text vs Wayfinder's own) -- same narrow, non-verbatim
# contract as `tests/error_shapes.rs`. Captured and checked directly by
# `qf_naming_one_undefined_field_among_valid_ones_400s` instead. Fixture:
# solr-ref/responses/edismax_qf_partial_invalid.json
# curl -sg "$EDISMAX_SOLR/$EDISMAX_CORE/select?q=rocket&defType=edismax&qf=title+nosuchfield&fl=id&wt=json"
# q grammar: quoted phrase, `+`/`-` operators
cape edismax_quoted_phrase      'select?q=%22quick+fox%22&defType=edismax&qf=body&fl=id&fq=id:(pA+OR+pB)&wt=json'
cape edismax_operators_exclude  'select?q=rocket+-mission&defType=edismax&qf=title+body&fl=id&fq=id:(eA+OR+eB+OR+eC+OR+eD)&wt=json'
cape edismax_operators_required 'select?q=%2Brocket+%2Blaunch&defType=edismax&qf=title+body&fl=id&fq=id:(eA+OR+eB+OR+eC+OR+eD)&wt=json'
# mm applied to a real query: "alpha beta gamma" (3 optional clauses) against
# mmA/mmB/mmC/mmD, which contain exactly 3/2/1/0 of those words
cape edismax_mm_1               'select?q=alpha+beta+gamma&defType=edismax&qf=body&mm=1&fl=id&fq=id:(mmA+OR+mmB+OR+mmC+OR+mmD)&wt=json'
cape edismax_mm_2               'select?q=alpha+beta+gamma&defType=edismax&qf=body&mm=2&fl=id&fq=id:(mmA+OR+mmB+OR+mmC+OR+mmD)&wt=json'
cape edismax_mm_3               'select?q=alpha+beta+gamma&defType=edismax&qf=body&mm=3&fl=id&fq=id:(mmA+OR+mmB+OR+mmC+OR+mmD)&wt=json'
cape edismax_mm_conditional     'select?q=alpha+beta+gamma&defType=edismax&qf=body&mm=2%3C-1+3%3C80%25&fl=id&fq=id:(mmA+OR+mmB+OR+mmC+OR+mmD)&wt=json'
# pf phrase-building over a negated clause (issue #114): the ticket assumed
# (its own wording: "presumably") that `pf`'s phrase should exclude a negated
# (`-term`) clause's text. A real-Solr capture (one-off container, same
# title/body schema as this block, plus two extra docs not in this script's
# corpus -- nA="rocket launch success"/nB="launch rocket success") disproves
# that: adding `-zzznonexistent` (absent from every doc) makes `pf`'s boost
# vanish completely, identical to the unboosted score nB already carries in
# the isolated capture. Consistent with real Solr's own `pf` folding a
# negated clause's text into the phrase it builds, same as Wayfinder's
# existing `literal_texts` today -- no divergence, no fix needed.
# Deliberately not `cape`/manifest.tsv calls: nA/nB aren't part of this
# script's shared corpus (added only in the test's own setup, to avoid
# perturbing numFound-sensitive fixtures already captured against it), so the
# generic hermetic sweep has nothing to index them against. Locked in by
# `pf_phrase_over_a_negated_absent_term_loses_its_boost_matching_solr`
# instead. Fixtures: solr-ref/responses/edismax_pf_negation_isolated.json,
# edismax_pf_negation_with_absent_negated_term.json
# curl -sf "$EDISMAX_SOLR/$EDISMAX_CORE/update?commit=true" -H 'Content-Type: application/json' -d '[{"id":"nA","title":"filler","body":"rocket launch success"},{"id":"nB","title":"filler","body":"launch rocket success"}]'
# curl -sg "$EDISMAX_SOLR/$EDISMAX_CORE/select?q=rocket+launch&defType=edismax&qf=body&pf=body&fl=id,score&fq=id:(nA+OR+nB)&wt=json"
# curl -sg "$EDISMAX_SOLR/$EDISMAX_CORE/select?q=rocket+launch+-zzznonexistent&defType=edismax&qf=body&pf=body&fl=id,score&fq=id:(nA+OR+nB)&wt=json"
# mm entirely absent vs mm= (empty string) (issue #113): CORRECTS the issue's
# own stated premise. Issue #113 assumed real Solr ignores an empty `mm` and
# falls back to its normal OR default, same as `mm` being absent entirely.
# Confirmed against real Solr (one-off container, same schema/corpus as this
# block -- not re-run through this script) that this is WRONG: `mm` absent
# does fall back to the normal OR default (`edismax_mm_absent` below, 200,
# numFound 3: mmA/mmB/mmC), but `mm=` (present, empty) does NOT silently
# fall back to anything -- it 400s with a NumberFormatException, same as any
# other malformed `mm` spec. `edismax_mm_absent` is a genuine manifest row
# (200, non-error). `edismax_mm_empty_string` is deliberately NOT a `cape`
# call / manifest.tsv row: it's an error envelope, and the generic hermetic
# sweep compares `error.msg`/`error.metadata` verbatim, which would always
# fail (Solr's Java exception text vs Wayfinder's own) -- same narrow,
# non-verbatim contract as `tests/error_shapes.rs` and issue #111's
# `edismax_qf_partial_invalid`. Captured and checked directly by
# `mm_present_but_empty_400s_like_a_malformed_spec` instead. Fixture:
# solr-ref/responses/edismax_mm_empty_string.json
# curl -sg "$EDISMAX_SOLR/$EDISMAX_CORE/select?q=alpha+beta+gamma&defType=edismax&qf=body&mm=&fl=id&wt=json"
cape edismax_mm_absent          'select?q=alpha+beta+gamma&defType=edismax&qf=body&fl=id&wt=json'
# Reviewer round-2 follow-up (issue #113): the 200/400 clause-count boundary
# above lived only in prose (finding 89, tests/edismax.rs comments) -- this is
# the primary committed-fixture evidence for one point on that boundary,
# `q=*:*` (a single-clause query that reaches the `mm=` guard's clauses.len()
# check with count < 2 and so must 200, not 400). numFound (10) is confirmed
# against a real one-off Solr capture already cited by the implementor
# (test comment on `empty_mm_alongside_a_single_clause_q_does_not_400`); the
# doc-order/id list in the committed fixture was NOT independently
# re-captured against live Solr for this task (no Docker container available)
# -- it was reconstructed from Wayfinder's own hermetic test run for this
# exact request and matches the insertion-order convention already observed
# in `edismax_mm_absent` above (a genuine real-Solr capture) for a
# no-sort/no-score match on a freshly built, unmerged segment. Re-running this
# `cape` call against a live container is the remaining step to fully close
# this gap; until then this fixture is corroborating, not independently
# verified, evidence for anything past `numFound`.
cape edismax_mm_empty_star      'select?q=*:*&defType=edismax&qf=body&mm=&fl=id&wt=json'

# --- admin system-info version handshake (issue #59) ------------------------
# `search_api_solr`'s `SolrConnector::getSolrVersion()` (finding 78) reads
# `lucene.solr-spec-version` off `<core>/admin/system`, falling back to
# `/admin/info/system`. `admin_system`/`admin_info_system` fixtures are
# manifest rows (`manifest.tsv`/`manifest-errors.tsv` respectively — the
# former is core-relative, the latter isn't, per CLAUDE.md's manifest
# contract), but they are deliberately NOT captured by `cap`/`capx` here: the
# ground truth for `core.schema` is the Search API connector's own generated
# schema (`drupal-4.4.0-solr-9.x-0`), captured separately against the
# `search_api_capture` core in `solr-ref/search-api/capture.sh` and already
# committed at `solr-ref/search-api/trace/00023.json`/`00026.json` (issue
# #55). This script's `$CORE` (`content`) runs the tracer-bullet schema, not
# the Drupal-generated one — a `cap`/`capx` call here would capture the
# *wrong* core's schema name and silently overwrite the real ground truth in
# `solr-ref/responses/admin_system.json`/`admin_info_system.json` on the next
# re-run (the exact fixture-corruption hazard CLAUDE.md warns about). Those
# two response files are verbatim copies of the trace files above instead.

# --- internal _version_ stats watermark (issue #99) -------------------------
# Appended block; own Solr 9 core and port. `_version_` is supplied by Solr's
# default schema, not configured here. The function parameter is intentionally
# included because search_api_solr sends this exact watermark request shape.
VERSION_CONTAINER=wayfinder-solr-99
VERSION_SOLR=http://localhost:8999/solr
VERSION_CORE=version99
if ! docker ps --format '{{.Names}}' | grep -qx "$VERSION_CONTAINER"; then
  docker rm -f "$VERSION_CONTAINER" >/dev/null 2>&1 || true
  docker run -d --name "$VERSION_CONTAINER" -p 8999:8983 \
    solr:9 solr-precreate "$VERSION_CORE" >/dev/null
fi
echo -n "waiting for version-field solr"
for _ in $(seq 60); do
  if curl -sf "$VERSION_SOLR/$VERSION_CORE/admin/ping?wt=json" >/dev/null 2>&1; then echo " ok"; break; fi
  echo -n "."; sleep 1
done
curl -sf "$VERSION_SOLR/$VERSION_CORE/update?commit=true" -H 'Content-Type: application/json' -d '[
  {"id":"v1"}, {"id":"v2"}, {"id":"v3"}
]' >/dev/null
# `function=max(_version_)` is accepted and echoed by Solr 9; the stats block
# remains keyed by stats.field and exposes its normal metrics, including max.
capv() {  # capv <name> <url-after-/solr/>
  local name=$1 suffix=$2
  want "$name" || return 0
  curl -sg "$VERSION_SOLR/$suffix" -o "$OUT/$name.json" -w '%{http_code}' > "$OUT/$name.status"
  printf '%s\t%s\t%s\t%s\t%s\t%s\n' \
    "$name" "$(cat "$OUT/$name.status")" GET "$suffix" "" "$VERSION_SOLR" \
    >> "$MANIFEST_ERRORS"
  rm -f "$OUT/$name.status"
}
capv stats_version_max "$VERSION_CORE/select?q=*:*&rows=0&stats=true&stats.field=_version_&function=max(_version_)&wt=json"
release "$VERSION_CONTAINER" "version-field core '$VERSION_CORE'"

# --- hl.fragsize=0 whole-field highlighting (issue #104) --------------------
# Appended block; own Solr 9 core and port. The shared corpus has no field
# long enough to distinguish hl.fragsize=0 (Solr's "whole field, no
# fragmenting" sentinel) from the default fragment width, so this indexes one
# ~300-char doc solely to make that distinguishable.
FRAGSIZE_CONTAINER=wayfinder-solr-104
FRAGSIZE_SOLR=http://localhost:8995/solr
FRAGSIZE_CORE=fragsize104
if ! docker ps --format '{{.Names}}' | grep -qx "$FRAGSIZE_CONTAINER"; then
  docker rm -f "$FRAGSIZE_CONTAINER" >/dev/null 2>&1 || true
  docker run -d --name "$FRAGSIZE_CONTAINER" -p 8995:8983 \
    solr:9 solr-precreate "$FRAGSIZE_CORE" >/dev/null
fi
echo -n "waiting for fragsize solr"
for _ in $(seq 60); do
  if curl -sf "$FRAGSIZE_SOLR/$FRAGSIZE_CORE/admin/ping?wt=json" >/dev/null 2>&1; then echo " ok"; break; fi
  echo -n "."; sleep 1
done
curl -s "$FRAGSIZE_SOLR/$FRAGSIZE_CORE/schema" -H 'Content-Type: application/json' -d '{
  "add-field": [
    {"name":"body", "type":"text_en", "indexed":true, "stored":true}
  ]
}' >/dev/null
curl -sf "$FRAGSIZE_SOLR/$FRAGSIZE_CORE/update?commit=true" -H 'Content-Type: application/json' -d '[
  {"id":"long1","body":"quick prototype notes from the engineering standup this morning. the team reviewed the roadmap for the next quarter and discussed several open risks around supply chain timing. afterwards everyone broke for lunch and reconvened at two in the afternoon to continue the planning session for the rest of the week."}
]' >/dev/null
capf() {  # capf <name> <url-after-/solr/>
  local name=$1 suffix=$2
  want "$name" || return 0
  curl -sg "$FRAGSIZE_SOLR/$suffix" -o "$OUT/$name.json" -w '%{http_code}' > "$OUT/$name.status"
  printf '%s\t%s\t%s\t%s\t%s\t%s\n' \
    "$name" "$(cat "$OUT/$name.status")" GET "$suffix" "" "$FRAGSIZE_SOLR" \
    >> "$MANIFEST_ERRORS"
  rm -f "$OUT/$name.status"
}
capf hl_fragsize_zero_whole_field "$FRAGSIZE_CORE/select?q=body:quick&hl=true&hl.fl=body&hl.fragsize=0&wt=json"
capf hl_fragsize_zero_whole_field_method_original "$FRAGSIZE_CORE/select?q=body:quick&hl=true&hl.fl=body&hl.method=original&hl.fragsize=0&wt=json"
capf hl_fragsize_small_truncated "$FRAGSIZE_CORE/select?q=body:quick&hl=true&hl.fl=body&hl.method=original&hl.fragsize=40&wt=json"
release "$FRAGSIZE_CONTAINER" "fragsize core '$FRAGSIZE_CORE'"

# --- q=*:* with a bad qf (issue #112) ---------------------------------------
# q=*:* with a bad qf: confirmed against real Solr (one-off container, same
# schema as the edismax block above -- not re-run through this script) that
# `qf` is validated before `q` is ever looked at, so an undefined `qf` field
# 400s even when `q=*:*` -- same narrow, non-verbatim contract as
# `tests/error_shapes.rs`, and deliberately not a `cape` call / manifest.tsv
# row for the same reason as `edismax_qf_partial_invalid` above. Checked
# directly by `star_query_with_undefined_qf_field_still_400s` and
# `star_query_with_partially_invalid_qf_still_400s`. Fixtures:
# solr-ref/responses/edismax_qf_star_unknown.json
# solr-ref/responses/edismax_qf_star_partial_invalid.json
# curl -sg "$EDISMAX_SOLR/$EDISMAX_CORE/select?q=*:*&defType=edismax&qf=nosuchfield&fl=id&wt=json"
# curl -sg "$EDISMAX_SOLR/$EDISMAX_CORE/select?q=*:*&defType=edismax&qf=title+nosuchfield&fl=id&wt=json"

# --- facet.field local-params key prefix (issue #138) -----------------------
# `search_api_solr` always sends facet.field={!key=X}field, and in every
# captured module request X is *identical* to the field name -- so the module
# traces alone cannot tell "uses the key" from "uses the field name". These
# five were captured against a one-off solr:9 container (port 8994) with a
# `content` core built from exactly the schema and corpus above, so the three
# manifest.tsv rows are core-relative GETs the differential harness can replay
# verbatim. The container was removed afterwards; re-take them the same way
# rather than re-running this whole script.
#
#   FKEY_SOLR=http://localhost:8994/solr; FKEY_CORE=content
#
# What they settle, all four of the issue's open questions:
#   1. The key is the response label: `{!key=mylabel}category` returns its
#      counts under "mylabel", not "category" (facet_local_params_key.json).
#      A key equal to the field name is the module's own shape and is a
#      visual no-op (facet_local_params_key_same.json).
#   2. `f.<field>.facet.*` overrides key off the *field*, not the local key:
#      with `{!key=mylabel}category`, `f.category.facet.missing=true` appends
#      the null bucket and `f.mylabel.facet.missing=true` does nothing.
#      Fixtures facet_local_params_key_f_field.json / _f_key.json -- evidence
#      for #140, which owns per-field overrides. Deliberately *not*
#      manifest.tsv rows: Wayfinder does not implement `f.<field>.facet.*`
#      yet, so a row would only buy an EXPECTED_DIVERGENCES entry in a file
#      #140 is about to touch anyway.
#   3/4. No captured module request puts a local-params prefix on
#      `facet.query`, `facet.pivot`, or `fq` -- descoped, not generalised.
# The unknown-field 400 names the *remainder*, not the key or the raw value:
# msg "undefined field: \"nosuchfield\"" (facet_local_params_key_unknown.json).
# cap facet_local_params_key         'select?q=*:*&rows=0&facet=true&facet.field=%7B%21key%3Dmylabel%7Dcategory&wt=json'
# cap facet_local_params_key_same    'select?q=*:*&rows=0&facet=true&facet.field=%7B%21key%3Dcategory%7Dcategory&wt=json'
# cap facet_local_params_key_unknown 'select?q=*:*&rows=0&facet=true&facet.field=%7B%21key%3Dk%7Dnosuchfield&wt=json'
# cap facet_local_params_key_f_field 'select?q=*:*&rows=0&facet=true&facet.field=%7B%21key%3Dmylabel%7Dcategory&f.category.facet.missing=true&wt=json'
# cap facet_local_params_key_f_key   'select?q=*:*&rows=0&facet=true&facet.field=%7B%21key%3Dmylabel%7Dcategory&f.mylabel.facet.missing=true&wt=json'
#
# Three more, taken the same way (one-off solr:9, port 8993) after stage 1
# flagged them as the assertions with no ground truth behind them:
#   - facet_local_params_key_as_other_field.json: `{!key=body}category` labels
#     the `category` counts "body". The key is *never* resolved as a field,
#     even when it names a different declared one -- which is what makes
#     "strip the prefix and use the field name" impossible to pass.
#   - facet_local_params_key_unterminated.json: `{!key=mylabel category` (no
#     closing brace) is a 400 SyntaxError, "Expected identifier at pos 22",
#     *not* a field name taken verbatim.
#   - facet_local_params_key_empty_remainder.json: `{!key=mylabel}` with
#     nothing after the brace is `undefined field: ""` -- the empty remainder
#     is what gets validated.
# Wayfinder 400s on the latter two as well, though with its own message; the
# differential harness tolerates `error.msg`, so their manifest rows pin the
# status only.
# cap facet_local_params_key_as_other_field  'select?q=*:*&rows=0&facet=true&facet.field=%7B%21key%3Dbody%7Dcategory&wt=json'
# cap facet_local_params_key_unterminated    'select?q=*:*&rows=0&facet=true&facet.field=%7B%21key%3Dmylabel%20category&wt=json'
# cap facet_local_params_key_empty_remainder 'select?q=*:*&rows=0&facet=true&facet.field=%7B%21key%3Dmylabel%7D&wt=json'

# --- repeated `add` command keys in one body (issue #154) ---------------------
# `search_api_solr`'s real /update body (search-api/trace/00001.json) repeats
# the top-level `add` key once per doc -- six times -- and no fixture here had
# ever repeated a command key, so nothing pinned what Solr does with the
# duplicates. These twelve were captured against a one-off solr:9 container
# (port 8992) with an `update9` core built from exactly the schema and u1..u5
# seed above; the container was removed afterwards; re-take them the same way
# rather than re-running this whole script.
#
# CONSEQUENCE, stated plainly: this is the only fully commented-out block in
# this file, so these twelve fixtures are the ONLY ones `capture.sh` cannot
# regenerate on its own. `R_SOLR`/`R_CORE` are undefined here and `set -u`
# would abort the run if the lines were live. To re-take them, copy the
# commands below into a scratch script that starts its own port-8992
# container and defines these two variables:
#
#   R_SOLR=http://localhost:8992/solr; R_CORE=update9
#
# What they settle (finding 96):
#   1. EVERY repeated `add` executes -- update_repeated_add_batch's two adds
#      both land (r1/alpha AND r2/bravo). Not last-wins, which is what a
#      `serde_json::Value` top-level parse would give.
#   2. Commands execute in BODY ORDER, not grouped by kind. A `delete` between
#      two adds sees the earlier one (r3 added then deleted -> gone), and a
#      `delete` BEFORE an add of the same id does not consume it (r4 deleted
#      then re-added -> present, title `echo`). An adds-then-deletes execution
#      order gets that second case wrong.
#   3. Two adds of the SAME id leave one doc, the LAST (`same id second`).
#   4. A malformed command aborts the whole body: a doc-less `add` is a 400
#      ("Missing solr document at [66]") and the VALID add before it never
#      lands (numFound 0) even under ?commit=true; likewise an unknown command
#      key ("Unknown command 'frobnicate' at [129]").
#
# The corpus selects are scoped to the ids each body touches rather than
# `q=*:*`: manifest-errors.tsv rows replay in sequence against one accumulated
# hermetic core in tests/differential.rs, so a whole-corpus count would pin
# this capture's fresh-core state and diverge there for no compatibility
# reason. POSTs, so all twelve rows are manifest-errors.tsv, never manifest.tsv.
# capup update_repeated_add_batch "$R_CORE/update?wt=json" \
#   '{"add":{"doc":{"id":"r1","body":"first repeated add","title":"alpha"}},"add":{"doc":{"id":"r2","body":"second repeated add","title":"bravo"}},"delete":{"id":"u2"},"commit":{}}'
# capu update_select_after_repeated_add_batch GET \
#   "$R_CORE/select?q=id:r1+OR+id:r2+OR+id:u2&fl=id,title&rows=20&sort=id+asc&wt=json"
# capup update_repeated_add_delete_between "$R_CORE/update?wt=json" \
#   '{"add":{"doc":{"id":"r3","body":"third","title":"charlie"}},"delete":{"id":"r3"},"add":{"doc":{"id":"r4","body":"fourth","title":"delta"}},"commit":{}}'
# capu update_select_after_repeated_add_delete_between GET \
#   "$R_CORE/select?q=id:r3+OR+id:r4&fl=id,title&rows=20&sort=id+asc&wt=json"
# capup update_repeated_add_delete_before "$R_CORE/update?wt=json" \
#   '{"delete":{"id":"r4"},"add":{"doc":{"id":"r4","body":"re-added","title":"echo"}},"commit":{}}'
# capu update_select_after_repeated_add_delete_before GET \
#   "$R_CORE/select?q=id:r4&fl=id,title&rows=20&sort=id+asc&wt=json"
# capup update_repeated_add_same_id "$R_CORE/update?wt=json" \
#   '{"add":{"doc":{"id":"r5","body":"same id first","title":"foxtrot"}},"add":{"doc":{"id":"r5","body":"same id second","title":"golf"}},"commit":{}}'
# capu update_select_after_repeated_add_same_id GET \
#   "$R_CORE/select?q=id:r5&fl=id,title,body&wt=json"
# capup update_repeated_add_missing_doc "$R_CORE/update?commit=true&wt=json" \
#   '{"add":{"doc":{"id":"r6","body":"valid","title":"hotel"}},"add":{}}'
# capu update_select_after_repeated_add_missing_doc GET \
#   "$R_CORE/select?q=id:r6&fl=id,title&wt=json"
# capup update_repeated_add_unknown_key "$R_CORE/update?commit=true&wt=json" \
#   '{"add":{"doc":{"id":"r7","body":"valid","title":"india"}},"add":{"doc":{"id":"r8","body":"valid","title":"juliett"}},"frobnicate":{}}'
# capu update_select_after_repeated_add_unknown_key GET \
#   "$R_CORE/select?q=id:r7+OR+id:r8&fl=id,title&sort=id+asc&wt=json"

# --- f.<field>.facet.missing precedence (issue #140) ------------------------
# Captured against a one-off solr:9 container (port 8992), same schema and
# 5-doc corpus as the reference "content" core above (category multiValued
# string, doc5 has no category) -- not runnable standalone, and the container
# was removed afterwards; re-take the same way rather than re-running this
# whole script.
#
#   F140_SOLR=http://localhost:8992/solr; F140_CORE=content
#
# What these settle (issue #140's open question 1 -- precedence):
#   - `f.category.facet.missing` ALWAYS wins over the global `facet.missing`,
#     unconditionally, not merely when the global is unset:
#     `facet.missing=true&f.category.facet.missing=false` drops the null
#     bucket (facet_missing_field_override_wins_over_global_true.json) and
#     `facet.missing=false&f.category.facet.missing=true` adds it
#     (facet_missing_field_override_wins_over_global_false.json).
#   - The per-field override also works with no global param present at all
#     (facet_missing_field_override_alone.json).
#   - `f.<field>.facet.missing` for a field that was not itself requested via
#     `facet.field` is silently inert -- no error, no effect on the fields
#     that were requested (facet_missing_field_override_unrelated_field_no_effect.json,
#     `f.body.facet.missing=true` alongside `facet.field=category` only).
#   - With two `facet.field` values and only one `f.<field>.facet.missing`
#     override, the override applies to its own field only -- the global
#     still governs every other field in the same request. `id` (always
#     present, so its null bucket is present-but-zero, per finding 41a's
#     unconditional-append rule) proves the global side is still live, not
#     just silently absent (facet_missing_field_override_mixed_multi_field.json).
# Deliberately *not* manifest.tsv rows -- but NOT for #138's reason. This
# block was written before the implementation and originally said "Wayfinder
# does not implement `f.<field>.facet.*` yet, so a row would only buy a
# mandatory EXPECTED_DIVERGENCES entry". Issue #140 then implemented
# `f.<field>.facet.missing` on this very branch, so that rationale expired the
# moment it landed.
#
# The reason they stay out is now narrower: a manifest row hands the whole
# response body to the differential harness, which compares facet bucket
# *ordering* verbatim, and that is a distinct question from the precedence
# semantics these five captures were taken to settle. Promoting them is a
# deliberate follow-up with its own risk, not a side effect of this issue.
#
# The compatibility claim is not unpinned by that: all five bodies are
# asserted whole, against these exact fixtures, by `assert_matches_fixture` in
# tests/facet_field_missing_override.rs. Note also that the *other*
# `f.<field>.facet.*` params (.limit, .mincount, .sort, .prefix) do remain
# unimplemented and still 400 under strict_params -- see PER_FIELD_PARAMS in
# src/lib.rs.
# cap facet_missing_field_override_wins_over_global_true             'select?q=*:*&rows=0&facet=true&facet.field=category&facet.missing=true&f.category.facet.missing=false&wt=json'
# cap facet_missing_field_override_wins_over_global_false            'select?q=*:*&rows=0&facet=true&facet.field=category&facet.missing=false&f.category.facet.missing=true&wt=json'
# cap facet_missing_field_override_alone                             'select?q=*:*&rows=0&facet=true&facet.field=category&f.category.facet.missing=true&wt=json'
# cap facet_missing_field_override_unrelated_field_no_effect         'select?q=*:*&rows=0&facet=true&facet.field=category&f.body.facet.missing=true&wt=json'
# cap facet_missing_field_override_mixed_multi_field                 'select?q=*:*&rows=0&facet=true&facet.field=category&facet.field=id&facet.missing=true&f.category.facet.missing=false&wt=json'

# --- MLT refinements: fq, mlt.match.include/offset, json.nl, fl=*,score (issue #141) ---
# `search_api_solr` sends five params `MLT_PARAMS` did not allowlist. These
# eight fixtures answer the open questions and pin ground truth for each,
# captured against a one-off `solr:9` container (port 8996, `wayfinder-solr-141`,
# removed afterwards) running the *same* schema/corpus as the MLT block above
# (own container because that one was not left running) -- reindexed with the
# exact same 20-doc corpus, no schema changes (fq/match tests reuse the
# existing `category` field rather than adding a new one).
#
# What they settle, mapped to the issue's five open questions:
#   1. `fq` on `/mlt` filters the *similar-docs* result set (`response`) only
#      -- it does NOT restrict which document `q` resolves as the seed
#      (`match`). `mlt_fq_scope.json` (fq=category:astronomy narrows the
#      astronomy cluster's 4 matches to 3, dropping mlt17/outdoors);
#      `mlt_fq_seed_not_filtered.json` (fq=category:cooking, which excludes
#      mlt11's own category, still resolves `match` to mlt11 -- only
#      `response` empties out); `mlt_fq_multiple_and.json` confirms two `fq`
#      params AND together (astronomy AND outdoors matches nothing, same as
#      `/select`).
#   2. Confirmed by reading Tantivy 0.26.1's `MoreLikeThis` struct source
#      directly (no capture needed): it has no maxNumTokensParsed-equivalent
#      field at all. `mlt_maxntp_noop.json` captures `mlt.maxntp=5000` (a
#      value far above this corpus's token counts) producing the exact same
#      result as the baseline `mlt.mintf=1&mlt.mindf=1&mlt.maxdf=10` query --
#      real Solr *can* narrow results with a low `mlt.maxntp`, so
#      accepted-and-ignore is a real capability gap, not a safe no-op in
#      general; this fixture only pins the realistic case where the corpus's
#      real Lucene token count sits under the value sent.
#   3. `fl=*,score` on `/mlt`: `mlt_fl_wildcard_score.json` shows real Solr
#      returning *every* stored/docValues field plus `score` -- confirming the
#      same wildcard-`fl` gap already exists on `/select` (see the closing
#      note of the issue #141 findings block): Wayfinder's `render_doc` treats `fl` as a literal field-name
#      allowlist, so `*` matches no real field and every field but `score`
#      gets dropped. Not `/mlt`-specific.
#   4. `mlt.match.include=false`: `mlt_match_include_false.json` shows the
#      `match` key is dropped from the envelope entirely -- not an
#      empty-and-present object.
#   5. `mlt.match.offset` is load-bearing, not cosmetic: `mlt_match_offset.json`
#      (`q=category:astronomy&mlt.match.offset=1`) resolves the *second*
#      match (mlt12) as the seed doc, not the first (mlt11) -- and
#      `match.start` reflects the offset (1), not always 0.
#   6. `json.nl` on `/mlt` only has anything to bite on when
#      `mlt.interestingTerms` is also requested and empty:
#      `mlt_json_nl_map_empty_terms.json` (`json.nl=map`, degenerate 0-term
#      doc) shows `interestingTerms` rendered as `{}`, not the default `[]`
#      -- so it is not purely cosmetic either, though the effect is narrow
#      (Wayfinder's `interestingTerms` is always `[]` today regardless of the
#      real term count -- a pre-existing, separate gap, findings 62/#141).
#
# Reproduce (one-off, not run by this script):
#   docker run -d --name wayfinder-solr-141 -p 8996:8983 solr:9 solr-precreate content
#   curl "$SOLR/schema" -d '{"add-field":[{"name":"body","type":"text_en","indexed":true,"stored":true},{"name":"category","type":"string","indexed":true,"stored":true,"docValues":true,"multiValued":true}]}'
#   curl "$SOLR/config" -d '{"add-requesthandler":{"name":"/mlt","class":"solr.MoreLikeThisHandler"}}'
#   # then index the MLT block's own 20-doc corpus (identical to above) and:
# capm mlt_fq_scope               'mlt?q=id:mlt11&mlt.fl=body&mlt.mintf=1&mlt.mindf=1&fq=category:astronomy&wt=json'
# capm mlt_fq_seed_not_filtered   'mlt?q=id:mlt11&mlt.fl=body&mlt.mintf=1&mlt.mindf=1&fq=category:cooking&wt=json'
# capm mlt_fq_multiple_and        'mlt?q=id:mlt11&mlt.fl=body&mlt.mintf=1&mlt.mindf=1&fq=category:astronomy&fq=category:outdoors&wt=json'
# capm mlt_match_include_false    'mlt?q=id:mlt11&mlt.fl=body&mlt.mintf=1&mlt.mindf=1&mlt.match.include=false&wt=json'
# capm mlt_match_offset           'mlt?q=category:astronomy&mlt.fl=body&mlt.match.offset=1&wt=json'
# capm mlt_json_nl_map_empty_terms 'mlt?q=id:mlt1&mlt.fl=body&mlt.interestingTerms=details&json.nl=map&wt=json'
# capm mlt_fl_wildcard_score      'mlt?q=id:mlt11&mlt.fl=body&mlt.mintf=1&mlt.mindf=1&fl=%2A%2Cscore&wt=json'
# capm mlt_maxntp_noop            'mlt?q=id:mlt11&mlt.fl=body&mlt.mintf=1&mlt.mindf=1&mlt.maxdf=10&mlt.maxntp=5000&wt=json'

# --- edismax: phrase-vs-OR and the Shape-B binding rule (issue #147) ---------
# Two edismax facts rested on documentation and inference rather than on a
# fixture, which inverts CLAUDE.md's "fixtures are ground truth" contract.
# Captured 2026-08-01 against a real `solr:9` using the edismax block above
# verbatim -- same container (`wayfinder-solr-7`, port 8994), same `content`
# core, same `title`/`body` text_en schema, same 10-doc corpus -- running only
# these three requests, not this script wholesale.
#
# 1. Unquoted multi-token clause: phrase or OR? `q=quick%2Brocket` is a single
#    clause (`+` is an ordinary term character mid-token in Lucene's
#    `_TERM_CHAR` set) whose `text_en` analysis yields two tokens, and no
#    document in the corpus has "quick" and "rocket" adjacent, so the two
#    readings are distinguishable: a `PhraseQuery` matches 0, a boolean OR
#    matches 6. Real Solr answered `numFound=6` (eA eB eC eD pA pB), settling
#    it as the OR reading and confirming `build_field_disjunction` (#137) plus
#    finding 92's documented-`autoGeneratePhraseQueries`-default inference.
#    `sort=id+asc` keeps the row safe for the hermetic sweep's exact document
#    order (PRD ratified-divergence 4 covers BM25 order).
cape edismax_unquoted_multitoken 'select?q=quick%2Brocket&defType=edismax&qf=title+body&fl=id&sort=id+asc&wt=json'
#
# 2/3. The Shape-B inline-nested-query binding rule, with `debugQuery=true`, one
#    capture per terminator `local_params::bound_token_len` implements. `qf`
#    names `title`/`body` while `df=id`, so the `parsedquery` says out loud
#    which clause the `+` bound to.
#
#    Deliberately NOT `cape` calls / manifest.tsv rows: Wayfinder emits no
#    `debug` section at all, so the whole-body hermetic sweep
#    (`hermetic_edismax_manifest_entries_match_committed_fixtures`) and
#    `tests/differential.rs` could only pass by widening a normaliser over a
#    real capability gap. Same deliberate exclusion as
#    `edismax_qf_partial_invalid` (#111), and
#    `shape_b_debugquery_captures_back_the_binding_rule_in_findings_90_and_91`
#    asserts neither appears in either manifest. Checked directly by
#    `tests/edismax.rs`'s `shape_b_debug_parsedquery_*` tests instead.
#
#    Whitespace terminator (trace 00003's shape). Captured:
#      parsedquery = (+(+DisjunctionMaxQuery((title:quick | body:quick)))) +id:rocket
#      numFound    = 0
#    "quick" fanned out over `qf`; "rocket", after the bound run, was resolved
#    by the outer lucene parser against `df=id` and matches nothing.
# curl -sg 'http://localhost:8994/solr/content/select?q=(%7B!edismax+qf%3D%27title+body%27%7D%2B%22quick%22+%2B%22rocket%22)&df=id&debugQuery=true&fl=id&sort=id+asc&wt=json' \
#   -o solr-ref/responses/edismax_shape_b_debug_parsedquery.json
#
#    `)`-at-run-local-paren-depth-0 terminator (trace 00006's shape, no
#    whitespace after `}` anywhere). Captured:
#      parsedquery = +(+DisjunctionMaxQuery((title:quick | body:quick)))
#      numFound    = 2 (pA pB)
#    The `)` closed the outer paren rather than being swallowed into the bound
#    run: a whitespace-only terminator would have handed the nested parser an
#    unbalanced `)`, and no `id:` clause came out of it either.
# curl -sg 'http://localhost:8994/solr/content/select?q=(%7B!edismax+qf%3D%27title+body%27%7D%2B%22quick%22)&df=id&debugQuery=true&fl=id&sort=id+asc&wt=json' \
#   -o solr-ref/responses/edismax_shape_b_debug_parsedquery_paren_terminated.json

# Released here rather than at the end of the edismax block above: the issue
# #147 section between them reuses this same container and core deliberately,
# so an earlier release would leave `cape edismax_unquoted_multitoken`
# capturing nothing.
release "$EDISMAX_CONTAINER" "edismax core '$EDISMAX_CORE'"

# --- edismax: `quick+rocket` is ONE clause, not two (issue #147, round 2) -----
# Capture 1 above settles phrase-vs-OR by `numFound` alone, which leaves the step
# *before* it -- "`quick+rocket` is one clause whose analysis yields two tokens",
# the `_TERM_CHAR` reading -- resting on the Lucene grammar rather than on a
# fixture. That step is what generalises the result to issue #137's actual
# `state-of-the-art` case, so it gets its own capture: capture 1's request again,
# with `debugQuery=true`. Captured 2026-08-01 against a real `solr:9` with the
# edismax block above verbatim (same `wayfinder-solr-7` container on port 8994,
# same `content` core, same `title`/`body` text_en schema, same 10-doc corpus),
# running only this one request -- this script was NOT re-run wholesale.
#
# Captured:
#   numFound             = 6 (eA eB eC eD pA pB), identical to capture 1
#   parsedquery          = +DisjunctionMaxQuery(((title:quick title:rocket) | (body:quick body:rocket)))
#   parsedquery_toString = +((title:quick title:rocket) | (body:quick body:rocket))
#   QParser              = ExtendedDismaxQParser
# Exactly ONE `DisjunctionMaxQuery`, spanning both tokens. Two clauses would have
# produced two of them, one per token
# (`+(DisjunctionMaxQuery((title:quick | body:quick)) DisjunctionMaxQuery((title:rocket | body:rocket)))`),
# so this discriminates the `_TERM_CHAR` reading from the two-clause one directly.
# It also shows the phrase-vs-OR answer structurally rather than only through a
# count: inside each `qf` field the two tokens are a SHOULD pair
# (`(title:quick title:rocket)`), not a `PhraseQuery`.
#
# Deliberately NOT a `cape` call / manifest.tsv row, for the same reason as
# captures 2/3 above: Wayfinder emits no `debug` section at all, so the
# whole-body sweeps could only pass by widening a normaliser over a real
# capability gap. Checked directly by
# `tests/edismax.rs::unquoted_multitoken_debug_parsedquery_shows_one_clause_over_both_tokens`.
# curl -sg 'http://localhost:8994/solr/content/select?q=quick%2Brocket&defType=edismax&qf=title+body&debugQuery=true&fl=id&sort=id+asc&wt=json' \
#   -o solr-ref/responses/edismax_unquoted_multitoken_debug.json

# --- colliding facet response keys (issue #149) -----------------------------
# Captured 2026-08-01 against a one-off `solr:9` container on port 8997
# (`wayfinder-solr-149`, removed afterwards), with the tracer-bullet schema and
# five-document corpus from the start of this script recreated verbatim. Only
# these four requests were run; this script was NOT re-run wholesale.
#
# Two `{!key=x}` facet fields make Solr's NamedList writer emit two literal `x`
# object members, in request order, under both the default and `json.nl=map`.
# This is legal to emit but ambiguous to consume: ordinary JSON object models
# retain only one member. By contrast, two identical `facet.query` values are
# coalesced by Solr itself into one `category:animals` member under both shapes.
#
# Deliberately NOT manifest.tsv rows. The differential harness parses each body
# into `serde_json::Value`, which necessarily discards one duplicate `x` member
# and would turn the field collision into a false-positive green. Dedicated raw
# fixture assertions in `tests/facet_key_collision.rs` preserve the evidence.
#
# Reproduce (after recreating the opening schema/corpus on port 8997):
# cap facet_collision_field_flat 'select?q=*:*&rows=0&facet=true&facet.field=%7B%21key%3Dx%7Dcategory&facet.field=%7B%21key%3Dx%7Did&wt=json'
# cap facet_collision_field_map  'select?q=*:*&rows=0&facet=true&facet.field=%7B%21key%3Dx%7Dcategory&facet.field=%7B%21key%3Dx%7Did&json.nl=map&wt=json'
# cap facet_collision_query_flat 'select?q=*:*&rows=0&facet=true&facet.query=category%3Aanimals&facet.query=category%3Aanimals&wt=json'
# cap facet_collision_query_map  'select?q=*:*&rows=0&facet=true&facet.query=category%3Aanimals&facet.query=category%3Aanimals&json.nl=map&wt=json'

# --- /terms differential coverage (issue #169) -----------------------------
# Captured 2026-08-01 against solr:9.10.1 on a clean `content` core with the
# tracer-bullet schema and five-document corpus from the start of this script.
# Unlike `/admin/mbeans`, this endpoint reports real index data and belongs in
# manifest.tsv. The capture settles the suspected analyzer mismatch: Solr stems
# `day` to `dai`, while Tantivy's English stemmer leaves `day` (issue #205).
cap terms_body 'terms?terms=true&terms.fl=body&omitHeader=true&wt=json'

# --- duplicate facet local-param keys (issue #150) --------------------------
# Captured 2026-08-01 against a one-off `solr:9` container on port 8998
# (`wayfinder-solr-150`, removed afterwards), with the tracer-bullet schema and
# five-document corpus from the start of this script recreated verbatim. Only
# this request was run; this script was NOT re-run wholesale.
#
# Contrary to the issue's source-based guess, Solr keeps the FIRST value when a
# local-param key is repeated: `{!key=a key=b}category` labels category's counts
# `a`, not `b`. Wayfinder's existing ordered-vector lookup already agrees; the
# fixture and regression test pin that behavior against future map rewrites.
# This is a core-relative GET with ordinary JSON, so its manifest.tsv row lets
# the differential harness replay it.
#
# Reproduce (after recreating the opening schema/corpus on port 8998):
# cap facet_local_params_duplicate_key 'select?q=*:*&rows=0&facet=true&facet.field=%7B%21key%3Da%20key%3Db%7Dcategory&wt=json'

# --- repeated json.nl resolution on /select (issue #153) -------------------
# Captured 2026-08-01 against a one-off `solr:9` container on port 8998
# (`wayfinder-solr-153`, removed afterwards), with the tracer-bullet schema and
# five-document corpus from the start of this script recreated verbatim. Only
# these two requests were run; this script was NOT re-run wholesale.
#
# The explicit-flat fixture confirms the alternating-array NamedList shape.
# Solr echoes both values in the repeated fixture's
# `responseHeader.params.json.nl`, but its shaped `facet_fields.category`
# object proves the first value (`map`) controls NamedList rendering over the
# later `flat` value.
#
# Reproduce (after recreating the opening schema/corpus on port 8998):
# cap select_json_nl_flat 'select?q=*:*&rows=0&facet=true&facet.field=category&json.nl=flat&wt=json'
# cap select_json_nl_repeated_map_flat 'select?q=*:*&rows=0&facet=true&facet.field=category&json.nl=map&json.nl=flat&wt=json'
# --- /update/extract exploration (issue #171) -------------------------------
# Captured 2026-08-01 against solr:9.10.1 with the first-party `extraction`
# module enabled. This is intentionally a separate core/container: the stock
# tracer-bullet core has no ExtractingRequestHandler, and indexing an extracted
# document mutates the corpus. The checked-in payloads are tiny, inspectable
# text fixtures; `broken.pdf` is deliberately malformed.
#
# These POSTs are multipart file uploads, so they cannot be represented by the
# JSON-body-only `capx`/`manifest-errors.tsv` runner. They also exercise a route
# Wayfinder does not implement yet, so putting them in the differential manifest
# would turn an exploration fixture into a permanent expected divergence. Keep
# the exact reproduction here until an implementation issue extends the runner.
EXTRACT_CONTAINER=wayfinder-solr-171
EXTRACT_SOLR=http://localhost:8998/solr
EXTRACT_CORE=extract171
# Always recreate this evidence core: accepting a same-named stale container
# could silently preserve a different image, handler, schema, or indexed doc.
docker rm -f "$EXTRACT_CONTAINER" >/dev/null 2>&1 || true
docker run -d --name "$EXTRACT_CONTAINER" -p 8998:8983 \
  -e SOLR_MODULES=extraction solr:9.10.1 solr-precreate "$EXTRACT_CORE" >/dev/null
extract_ready=false
for _ in $(seq 60); do
  if curl -sf "$EXTRACT_SOLR/$EXTRACT_CORE/admin/ping?wt=json" >/dev/null 2>&1; then
    extract_ready=true
    break
  fi
  sleep 1
done
if [ "$extract_ready" != true ]; then
  echo "extract Solr did not become ready" >&2
  exit 1
fi
curl -sSf "$EXTRACT_SOLR/$EXTRACT_CORE/config" -H 'Content-Type: application/json' -d '{
  "add-requesthandler": {
    "name":"/update/extract",
    "class":"solr.extraction.ExtractingRequestHandler",
    "startup":"lazy",
    "defaults": {
      "lowernames":"true", "uprefix":"ignored_", "captureAttr":"true",
      "fmap.a":"links", "fmap.div":"ignored_"
    }
  }
}' >/dev/null
curl -sSf "$EXTRACT_SOLR/$EXTRACT_CORE/schema" -H 'Content-Type: application/json' -d '{
  "add-field": [
    {"name":"body", "type":"text_en", "indexed":true, "stored":true},
    {"name":"links", "type":"strings", "indexed":true, "stored":true}
  ]
}' >/dev/null

cap_extract() { # cap_extract <name> <expected-status> <query> <input> [mime]
  local name=$1 expected=$2 query=$3 input=$4 mime=${5:-application/octet-stream} actual
  want "$name" || return 0
  actual=$(curl -sS -X POST "$EXTRACT_SOLR/$EXTRACT_CORE/update/extract?$query" \
    -F "file=@$HERE/extract-inputs/$input;type=$mime;filename=$input" \
    -o "$OUT/$name.json" -w '%{http_code}')
  if [ "$actual" != "$expected" ]; then
    echo "$name: expected HTTP $expected, got $actual" >&2
    exit 1
  fi
}
cap_extract extract_plain_text_xml 200 \
  'extractOnly=true&resource.name=sample.txt&wt=json' sample.txt
cap_extract extract_plain_text_text 200 \
  'extractOnly=true&extractFormat=text&resource.name=sample.txt&wt=json' sample.txt
cap_extract extract_html_index 200 \
  'literal.id=extract-html-captured&fmap.content=body&commit=true&resource.name=sample.html&wt=json' \
  sample.html
# The one capture that does not go through a `cap*` helper, so it carries its
# own `want` guard -- without it a filtered run would rewrite this fixture.
if want extract_html_select; then
  extract_select_status=$(curl -sS \
    "$EXTRACT_SOLR/$EXTRACT_CORE/select?q=id:extract-html-captured&fl=id,body,links&wt=json" \
    -o "$OUT/extract_html_select.json" -w '%{http_code}')
  if [ "$extract_select_status" != 200 ]; then
    echo "extract_html_select: expected HTTP 200, got $extract_select_status" >&2
    exit 1
  fi
fi
cap_extract extract_corrupt_pdf 500 \
  'extractOnly=true&extractFormat=text&resource.name=broken.pdf&wt=json' broken.pdf

release "$EXTRACT_CONTAINER" "extract core '$EXTRACT_CORE'"

# --- hl.fl=* over a stored string field (issue #184) ------------------------
# Captured 2026-08-01 against a clean one-off `solr:9` container on port 8999
# (`wayfinder-solr-184`), with the tracer-bullet schema and five-document corpus
# from the start of this script recreated verbatim. This request discriminates
# wildcard inclusion from exclusion: `category` is stored, and its `animals`
# value is also the query term. Solr keys both matching docs' highlighting on
# `category`, exactly as it does when `hl.fl=category` is explicit.
cap hl_wildcard_stored_string 'select?q=category%3Aanimals&hl=true&hl.fl=%2A&wt=json'

# --- dotted dynamic field names (issue #177) --------------------------------
# Captured against real Solr 9 to replace issue #164's Tantivy-source-derived
# assumption with wire evidence. Solr accepts ordinary, leading, trailing, and
# consecutive dots in names matched by a dynamic-field rule, and all four names
# remain queryable. The unstored rule and values are seeded with the canonical
# schema/corpus above so live manifest replay sees no overwrite history.
cap dotted_dynamic_basic       'select?q=tm_X3b_en_a.b:gamma&fl=id&sort=id+asc&wt=json'
cap dotted_dynamic_leading     'select?q=tm_X3b_en_.leading:gamma&fl=id&sort=id+asc&wt=json'
cap dotted_dynamic_trailing    'select?q=tm_X3b_en_trailing.:gamma&fl=id&sort=id+asc&wt=json'
cap dotted_dynamic_consecutive 'select?q=tm_X3b_en_a..b:gamma&fl=id&sort=id+asc&wt=json'

# --- omitHeader on errors and boolean spellings (issue #179) ---------------
# Captured 2026-08-01 against `solr:9.10.1` in a clean one-off container
# (`wayfinder-solr-179`, port 9010, removed afterwards). No schema or corpus is
# needed: querying an undefined field supplies the stable 400. All three JSON
# rows belong in manifest-errors.tsv because they are error responses, even though
# they are core-relative GETs.
#
# `true` settles this issue's original question: Solr suppresses
# `responseHeader` on errors too. `yes` proves omitHeader uses Solr's
# case-insensitive true/yes/on boolean vocabulary rather than exact `true`.
capx omit_header_error_true GET "$CORE/select?q=nosuchfield:x&omitHeader=true&wt=json"
capx omit_header_error_yes  GET "$CORE/select?q=nosuchfield:x&omitHeader=yes&wt=json"
capx omit_header_update_error_true POST "$CORE/update?omitHeader=true&wt=json" '{not json'
#
# The issue comment's claim that Solr also accepts `1` and `t` is wrong for
# Solr 9.10.1: both fail before the JSON response writer with Jetty HTML and
# `invalid boolean value`. Preserve the raw `1` evidence outside the manifest,
# whose differential runner requires JSON. Reproduce it with:
# curl -sg "$SOLR/$CORE/select?q=nosuchfield:x&omitHeader=1&wt=json" \
#   -o "$OUT/omit_header_invalid_one.html"

# --- highlighting true paths (issue #181) ----------------------------------
# Captured 2026-08-01 against a one-off `solr:9` container on port 9011
# (`wayfinder-solr-181`, removed afterwards). The dedicated corpus makes each
# true path visibly differ from its false control. These are deliberately not
# manifest.tsv rows: the differential core does not contain these documents,
# while focused fixture-backed tests recreate this exact schema and corpus.
#
# Reproduce:
#   docker run -d --name wayfinder-solr-181 -p 9011:8983 solr:9 solr-precreate content
#   curl "$SOLR/schema" -d '{"add-field":[{"name":"title","type":"text_en","indexed":true,"stored":true},{"name":"body","type":"text_en","indexed":true,"stored":true}]}'
#   curl "$SOLR/update?commit=true" -d '[{"id":"rfm1","title":"quick launch","body":"quick fox"},{"id":"rfm2","title":"quiet launch","body":"quick fox"},{"id":"merge1","title":"merge probe","body":"alpha one two three four five six seven eight nine ten eleven twelve beta thirteen fourteen fifteen sixteen seventeen eighteen nineteen twenty gamma"}]'
# where SOLR=http://localhost:9011/solr/content and both writes use
# `Content-Type: application/json`. Then capture:
#   select?q=title:quick%20OR%20body:fox&fl=id&sort=id%20asc&hl=true&hl.fl=title,body&hl.requireFieldMatch=false&wt=json
#   select?q=title:quick%20OR%20body:fox&fl=id&sort=id%20asc&hl=true&hl.fl=title,body&hl.requireFieldMatch=true&wt=json
#   select?q=body:(alpha%20beta%20gamma)&fq=id:merge1&fl=id&hl=true&hl.fl=body&hl.method=original&hl.fragsize=20&hl.snippets=5&hl.mergeContiguous=false&wt=json
#   select?q=body:(alpha%20beta%20gamma)&fq=id:merge1&fl=id&hl=true&hl.fl=body&hl.method=original&hl.fragsize=20&hl.snippets=5&hl.mergeContiguous=true&wt=json

# --- edismax residual grammar evidence (issue #197) -------------------------
# Captured 2026-08-01 against the existing edismax block's real `solr:9`
# container (`wayfinder-solr-7`, port 8994), `content` core, schema, and 10-doc
# corpus. These are commented one-off commands rather than `cape` calls:
# Wayfinder emits no `debug` section, and the nested request intentionally 400s,
# so neither belongs in manifest.tsv's differential whole-body sweep.
#
# 1. Finding 92 previously generalised the captured mid-token `+` grammar fact
#    to `-`. The motivating form from #137 now has direct parse-tree evidence:
#      parsedquery = +DisjunctionMaxQuery(((title:state title:art) | (body:state body:art)))
#    Exactly one dismax clause spans both analysed tokens; the two hyphens did
#    not split `state-of-the-art` into separate query clauses.
# curl -sg 'http://localhost:8994/solr/content/select?q=state-of-the-art&defType=edismax&qf=title+body&debugQuery=true&fl=id&sort=id+asc&wt=json' \
#   -o solr-ref/responses/edismax_midtoken_minus_debug.json
#
# 2. Finding 91's whitespace terminator had only been captured at bound-run
#    paren depth zero. Here the run starts `(+"quick"`, so its first whitespace
#    is at depth one. Real Solr answers 400: cutting there hands the outer parser
#    the unbalanced remainder `+"fox"))`. A depth-zero-only rule would instead
#    bind the complete balanced `(+"quick" +"fox")` expression and return 200.
# curl -sg 'http://localhost:8994/solr/content/select?q=(%7B!edismax+qf%3D%27title+body%27%7D(%2B%22quick%22+%2B%22fox%22))&df=id&debugQuery=true&fl=id&sort=id+asc&wt=json' \
#   -o solr-ref/responses/edismax_shape_b_debug_nested_paren.json
# --- boolean param parsing (issue #187) -------------------------------------
# Captured 2026-08-01 against a one-off `solr:9` container on port 8996
# (`wayfinder-solr-187`, removed afterwards), with the tracer-bullet schema and
# five-document corpus from the start of this script recreated verbatim. Only
# these requests were run; this script was NOT re-run wholesale.
#
# The issue's premise is wrong. Solr 9's `StrUtils.parseBool` does NOT accept
# `1`/`0`/`t`/`f`/`y`; those are a 400. What it does, on the lowercased value:
#   true  if it starts with `true`, `on` or `yes`   (so `truestuff` is true)
#   false if it starts with `false` or `off`, or equals `no` exactly
#         (so `offside` is false but `noo` is a 400)
#   otherwise 400 `invalid boolean value: <value>`
# Case-insensitive throughout: `TRUE`, `Yes`, `oN` all parse.
#
# Where the error surfaces depends on when the param is read: `facet=1` is read
# before the base query and yields an error-only envelope, while
# `facet.missing=nope` is read inside faceting and yields Solr's issue-#35
# shape -- `response` block alongside `error`.
#
# Not captured as a fixture: `omitHeader=1`. Header suppression is decided
# before the response writer exists, so Solr answers with a Jetty HTML error
# page, not a JSON envelope. Wayfinder deliberately answers with its ordinary
# JSON 400 there (documented divergence, see docs/solr-ref-findings.md).
#
# The port-8996 container above is provenance only -- how these nine were
# actually captured, without re-running this script wholesale. They are live
# `cap` calls, not commented-out reproduction notes, because this script opens
# with `rm -rf "$OUT"` and truncates manifest.tsv: as comments, the next full
# re-run would silently delete all nine fixtures and their manifest rows. Every
# one is an ordinary core-relative GET against the schema and corpus at the top
# of this script, so a wholesale re-run reproduces them exactly.
BOOL_BASE='select?q=*:*&rows=0&facet=true&facet.field=category'
cap bool_facet_missing_upper_true "$BOOL_BASE&facet.missing=TRUE&wt=json"
cap bool_facet_missing_yes        "$BOOL_BASE&facet.missing=yes&wt=json"
cap bool_facet_missing_on         "$BOOL_BASE&facet.missing=on&wt=json"
cap bool_facet_missing_no         "$BOOL_BASE&facet.missing=no&wt=json"
cap bool_facet_missing_prefix     "$BOOL_BASE&facet.missing=truestuff&wt=json"
cap bool_facet_missing_invalid    "$BOOL_BASE&facet.missing=nope&wt=json"
cap bool_facet_on      'select?q=*:*&rows=0&facet=on&facet.field=category&wt=json'
cap bool_facet_invalid 'select?q=*:*&rows=0&facet=1&facet.field=category&wt=json'
cap bool_omit_header_yes 'select?q=*:*&rows=0&omitHeader=yes&wt=json'

# --- partial `fl=ss_*` pattern expansion (issue #196) ----------------------
# A Search API configset/corpus probe must not touch the tracer-bullet core:
# its schema and corpus differ, and this fixture is not a manifest row because
# the differential harness cannot replay it against that core. Recreate this
# one-off container every run so the capture is self-contained and stable.
FL196_CONTAINER=wayfinder-solr-196
FL196_PORT=9000
FL196_CORE=search_api_fl_196
FL196_SOLR=http://127.0.0.1:$FL196_PORT/solr
FL196_CONFIGSET=/opt/solr/server/solr/configsets/search-api
FL196_CORPUS=$(mktemp)
docker rm -f "$FL196_CONTAINER" >/dev/null 2>&1 || true
docker run -d --name "$FL196_CONTAINER" -p "127.0.0.1:$FL196_PORT:8983" \
  -e SOLR_MODULES=analysis-extras \
  -v "$HERE/search-api/configset:$FL196_CONFIGSET:ro" \
  solr:9 solr-precreate "$FL196_CORE" "$FL196_CONFIGSET" >/dev/null
printf 'waiting for issue #196 Solr'
for _ in $(seq 60); do
  if curl -sf "$FL196_SOLR/$FL196_CORE/admin/ping?wt=json" >/dev/null 2>&1; then
    echo ' ok'
    break
  fi
  printf '.'
  sleep 1
done
curl -sf "$FL196_SOLR/$FL196_CORE/admin/ping?wt=json" >/dev/null
jq -r '.request.body' "$HERE/search-api/trace/00001.json" > "$FL196_CORPUS"
curl -sf "$FL196_SOLR/$FL196_CORE/update?commit=true&wt=json" \
  -H 'Content-Type: application/json' --data-binary "@$FL196_CORPUS" >/dev/null
rm -f "$FL196_CORPUS"
curl -sSf \
  "$FL196_SOLR/$FL196_CORE/select?q=ss_field_sku%3AART-001&fl=ss_%2A&rows=1&wt=json&omitHeader=true" \
  -o "$OUT/select_fl_ss_wildcard.json"
echo "captured select_fl_ss_wildcard.json from '$FL196_CONTAINER' (port $FL196_PORT)"
docker rm -f "$FL196_CONTAINER" >/dev/null

# --- mlt.maxntp token cap and Java-int errors (issue #189) ------------------
# Captured 2026-08-01 against clean one-off `solr:9` containers after
# recreating the MLT block's schema, handler, and 20-document corpus verbatim.
# `mlt_maxntp_low` used `wayfinder-solr-189`; the signed-int edge captures used
# `wayfinder-solr-189-edges`. Both containers were removed afterwards.
#
# Reproduce after setting MLT_SOLR to that recreated core and defining capm as
# in the issue #141 block:
# capm mlt_maxntp_low 'mlt?q=id:mlt11&mlt.fl=body&mlt.mintf=1&mlt.mindf=1&mlt.maxdf=10&mlt.maxntp=1&wt=json'
# curl -sS "$MLT_SOLR/mlt?q=id:mlt11&mlt.fl=body&mlt.maxntp=abc&wt=json" -o "$OUT/mlt_maxntp_invalid.json"
# curl -sS "$MLT_SOLR/mlt?q=id:mlt11&mlt.fl=body&mlt.maxntp=2147483648&wt=json" -o "$OUT/mlt_maxntp_overflow.json"
# A separate handler-only check against `wayfinder-solr-189-precedence`
# established that malformed `q=body:[` wins over malformed `mlt.maxntp=abc`.

# --- configured spellchecker suggestions and collations (issue #223) -------
# This is intentionally a dedicated Search API configset core: the canonical
# `content` core has no SpellCheckComponent, while this issue needs real term
# dictionaries for both configured `en` and `und` spellcheckers. The tiny
# corpus makes the dictionaries disagree (`quick` vs `quack`) so repeated
# dictionary precedence is observable rather than inferred.
SPELL_CONTAINER=wayfinder-solr-223
SPELL_PORT=9012
SPELL_CORE=spellcheck_223
SPELL_SOLR=http://127.0.0.1:$SPELL_PORT/solr
SPELL_CONFIGSET=/opt/solr/server/solr/configsets/search-api

docker rm -f "$SPELL_CONTAINER" >/dev/null 2>&1 || true
docker run -d --name "$SPELL_CONTAINER" -p "127.0.0.1:$SPELL_PORT:8983" \
  -e SOLR_MODULES=analysis-extras \
  -v "$HERE/search-api/configset:$SPELL_CONFIGSET:ro" \
  solr:9 solr-precreate "$SPELL_CORE" "$SPELL_CONFIGSET" >/dev/null
printf 'waiting for issue #223 Solr'
for _ in $(seq 60); do
  if curl -sf "$SPELL_SOLR/$SPELL_CORE/admin/ping?wt=json" >/dev/null 2>&1; then
    echo ' ok'
    break
  fi
  printf '.'
  sleep 1
done
curl -sf "$SPELL_SOLR/$SPELL_CORE/admin/ping?wt=json" >/dev/null
curl -sf "$SPELL_SOLR/$SPELL_CORE/update?commit=true&wt=json" \
  -H 'Content-Type: application/json' -d '[
    {"id":"s1","spellcheck_en":["quick quick quick rocket rocket"],"spellcheck_und":["quack quack quack garden"]},
    {"id":"s2","spellcheck_en":["quick brown fox"],"spellcheck_und":["quack garden"]}
  ]' >/dev/null

capspell() { # capspell <name> <query-after-select?>
  # Two `local`s, not one: bash expands every word of a `local` statement before
  # any of its assignments take effect, so `url=...$query` on one line reads the
  # *outer* `query` and dies under `set -u` (shellcheck SC2318). This function
  # could never have run as written.
  local name=$1 query=$2
  local url="$SPELL_CORE/select?$query"
  want "$name" || return 0
  curl -sg "$SPELL_SOLR/$url" -o "$OUT/$name.json" \
    -w '%{http_code}' > "$OUT/$name.status"
  printf '%s\t%s\tGET\t%s\t\t%s\n' \
    "$name" "$(cat "$OUT/$name.status")" "$url" "$SPELL_SOLR" \
    >> "$MANIFEST_ERRORS"
  rm -f "$OUT/$name.status"
}

SPELL_BASE='q=*:*&rows=0&wt=json&omitHeader=true&spellcheck=true'
capspell spellcheck_flat \
  "$SPELL_BASE&spellcheck.q=qwick%20roket&spellcheck.dictionary=en&spellcheck.collate=true&json.nl=flat"
capspell spellcheck_map \
  "$SPELL_BASE&spellcheck.q=qwick%20roket&spellcheck.dictionary=en&spellcheck.collate=true&json.nl=map"
capspell spellcheck_dictionary_en_first \
  "$SPELL_BASE&spellcheck.q=qwick&spellcheck.dictionary=en&spellcheck.dictionary=und&spellcheck.collate=true&json.nl=flat"
capspell spellcheck_dictionary_und_first \
  "$SPELL_BASE&spellcheck.q=qwick&spellcheck.dictionary=und&spellcheck.dictionary=en&spellcheck.collate=true&json.nl=flat"
capspell spellcheck_unicode_offsets \
  "$SPELL_BASE&spellcheck.q=%C3%A9%20qwick&spellcheck.dictionary=en&spellcheck.collate=true&json.nl=flat"

echo "captured issue #223 spellcheck fixtures from '$SPELL_CONTAINER' (port $SPELL_PORT)"
docker rm -f "$SPELL_CONTAINER" >/dev/null

# --- /update/extract extractOnly: HTML + charset ground truth (issue #258) ---
# Captured 2026-08-02 against solr:9.10.1 with the `extraction` module and the
# same Search-API-shaped ExtractingRequestHandler as the #171 block above.
# Separate container/core/port for the same reason #171 used one: the stock
# tracer-bullet core has no ExtractingRequestHandler.
#
# #171 captured extractOnly only for plain text; the HTML captures it took were
# the *indexing* path, which returns a bare responseHeader. Issue #258
# implements the extractOnly route for plain text AND HTML, so the HTML
# extractOnly envelope needed ground truth of its own rather than being
# inferred from the indexing path. The three charset rows pin the declared-vs-
# detected precedence the issue's charset handling depends on.
#
# Like #171's block these are multipart POSTs; they live in
# `solr-ref/manifest-multipart.tsv`, which the differential harness runs with a
# multipart-aware runner (`manifest-errors.tsv` models JSON bodies only).
EXTRACT258_CONTAINER=wayfinder-solr-258
EXTRACT258_SOLR=http://localhost:9020/solr
EXTRACT258_CORE=extract258
docker rm -f "$EXTRACT258_CONTAINER" >/dev/null 2>&1 || true
docker run -d --name "$EXTRACT258_CONTAINER" -p 9020:8983 \
  -e SOLR_MODULES=extraction solr:9.10.1 solr-precreate "$EXTRACT258_CORE" >/dev/null
extract258_ready=false
for _ in $(seq 90); do
  if curl -sf "$EXTRACT258_SOLR/$EXTRACT258_CORE/admin/ping?wt=json" >/dev/null 2>&1; then
    extract258_ready=true
    break
  fi
  sleep 1
done
if [ "$extract258_ready" != true ]; then
  echo "extract258 Solr did not become ready" >&2
  exit 1
fi
curl -sSf "$EXTRACT258_SOLR/$EXTRACT258_CORE/config" -H 'Content-Type: application/json' -d '{
  "add-requesthandler": {
    "name":"/update/extract",
    "class":"solr.extraction.ExtractingRequestHandler",
    "startup":"lazy",
    "defaults": {
      "lowernames":"true", "uprefix":"ignored_", "captureAttr":"true",
      "fmap.a":"links", "fmap.div":"ignored_"
    }
  }
}' >/dev/null

cap_extract258() { # cap_extract258 <name> <expected-status> <query> <input> [mime]
  local name=$1 expected=$2 query=$3 input=$4 mime=${5:-application/octet-stream} actual
  want "$name" || return 0
  actual=$(curl -sS -X POST "$EXTRACT258_SOLR/$EXTRACT258_CORE/update/extract?$query" \
    -F "file=@$HERE/extract-inputs/$input;type=$mime;filename=$input" \
    -o "$OUT/$name.json" -w '%{http_code}')
  if [ "$actual" != "$expected" ]; then
    echo "$name: expected HTTP $expected, got $actual" >&2
    exit 1
  fi
}

cap_extract258 extract_html_only_xml 200 \
  'extractOnly=true&resource.name=sample.html&wt=json' sample.html text/html
cap_extract258 extract_html_only_text 200 \
  'extractOnly=true&extractFormat=text&resource.name=sample.html&wt=json' sample.html text/html
cap_extract258 extract_latin1_text 200 \
  'extractOnly=true&extractFormat=text&resource.name=sample-latin1.txt&wt=json' \
  sample-latin1.txt
cap_extract258 extract_utf8_bom_text 200 \
  'extractOnly=true&extractFormat=text&resource.name=sample-utf8-bom.txt&wt=json' \
  sample-utf8-bom.txt
cap_extract258 extract_declared_charset_text 200 \
  'extractOnly=true&extractFormat=text&resource.name=sample-latin1.txt&wt=json' \
  sample-latin1.txt 'text/plain; charset=ISO-8859-1'

echo "captured issue #258 extractOnly fixtures from '$EXTRACT258_CONTAINER' (port 9020)"
docker rm -f "$EXTRACT258_CONTAINER" >/dev/null

# --- /update/extract PDF corpus (issue #261) ---------------------------------
# Captured 2026-08-03 against solr:9.10.1 with the `extraction` module and the
# same Search-API-shaped ExtractingRequestHandler as the #171/#258 blocks.
# Separate container/core/port for the same reason: no shared state, no churn
# of existing fixtures. These are the born-digital PDF evaluation corpus from
# the #261 exploration report (docs/reports/2026-08-03-pdf-extraction-corpus.md):
# one fixture per corpus file, captured as Tika ground truth for the go/no-go.
#
# Corpus provenance (all generated, all redistributable; fonts are DejaVu,
# Bitstream-Vera-family, embedded as subsets by WeasyPrint/HarfBuzz so the
# ToUnicode CMaps and OpenType ligatures are the same shape Word/LibreOffice
# emit): see the report. Files live in solr-ref/extract-inputs/pdf-*.pdf.
#
# Like #171/#258 these are multipart POSTs and stay OUT of manifest-multipart.tsv
# for now: Wayfinder has no PDF extractor yet, so adding them there would turn
# exploration evidence into permanent expected divergences. The implementation
# issue (the #261 go-issue follow-up) extends the runner and adds the rows.
EXTRACT261_CONTAINER=wayfinder-solr-261
EXTRACT261_SOLR=http://localhost:9030/solr
EXTRACT261_CORE=extract261
docker rm -f "$EXTRACT261_CONTAINER" >/dev/null 2>&1 || true
docker run -d --name "$EXTRACT261_CONTAINER" -p 9030:8983 \
  -e SOLR_MODULES=extraction solr:9.10.1 solr-precreate "$EXTRACT261_CORE" >/dev/null
extract261_ready=false
for _ in $(seq 90); do
  if curl -sf "$EXTRACT261_SOLR/$EXTRACT261_CORE/admin/ping?wt=json" >/dev/null 2>&1; then
    extract261_ready=true
    break
  fi
  sleep 1
done
if [ "$extract261_ready" != true ]; then
  echo "extract261 Solr did not become ready" >&2
  exit 1
fi
curl -sSf "$EXTRACT261_SOLR/$EXTRACT261_CORE/config" -H 'Content-Type: application/json' -d '{
  "add-requesthandler": {
    "name":"/update/extract",
    "class":"solr.extraction.ExtractingRequestHandler",
    "startup":"lazy",
    "defaults": {
      "lowernames":"true", "uprefix":"ignored_", "captureAttr":"true",
      "fmap.a":"links", "fmap.div":"ignored_"
    }
  }
}' >/dev/null

cap_extract261() { # cap_extract261 <name> <expected-status> <input>
  local name=$1 expected=$2 input=$3 actual
  want "$name" || return 0
  local query="extractOnly=true&extractFormat=text&resource.name=$input&wt=json"
  actual=$(curl -sS -X POST "$EXTRACT261_SOLR/$EXTRACT261_CORE/update/extract?$query" \
    -F "file=@$HERE/extract-inputs/$input;type=application/pdf;filename=$input" \
    -o "$OUT/$name.json" -w '%{http_code}')
  if [ "$actual" != "$expected" ]; then
    echo "$name: expected HTTP $expected, got $actual" >&2
    exit 1
  fi
}

# Success cases (200): subset font + ToUnicode, ligatures, multi-column, a
# multi-page document (the per-page checkpoint needs >1 page), the
# Info-dict-vs-XMP metadata-conflict document, and the image-only "scanned"
# page (no text layer -> legitimate empty body, no OCR).
cap_extract261 extract_pdf_embedded_font     200 pdf-embedded-font.pdf
cap_extract261 extract_pdf_ligatures         200 pdf-ligatures.pdf
cap_extract261 extract_pdf_multicolumn       200 pdf-multicolumn.pdf
cap_extract261 extract_pdf_multipage         200 pdf-multipage.pdf
cap_extract261 extract_pdf_metadata_conflict 200 pdf-metadata-conflict.pdf
cap_extract261 extract_pdf_image_only        200 pdf-image-only.pdf
# Failure cases (500): an AES-encrypted PDF posted with no password (the wire
# shape -- the client never sends one) and a structurally-valid PDF with a
# corrupted content stream. Both are captured in the same Solr error envelope
# as extract_corrupt_pdf.json (SolrException root).
cap_extract261 extract_pdf_encrypted         500 pdf-encrypted.pdf
cap_extract261 extract_pdf_malformed_objects 500 pdf-malformed-objects.pdf

echo "captured issue #261 PDF corpus fixtures from '$EXTRACT261_CONTAINER' (port 9030)"
docker rm -f "$EXTRACT261_CONTAINER" >/dev/null

# --- /update/extract extractOnly: office/ODF/RTF (issue #260) --------------
# Captured 2026-08-02 against solr:9.10.1 with the `extraction` module and the
# same ExtractingRequestHandler config #171/#258 use. The office payloads are
# tiny, inspectable documents generated by `solr-ref/extract-inputs/_gen_office.py`
# (python-docx/python-pptx/openpyxl/odfpy + a hand-written RTF); the `broken.*`
# inputs are truncated archives (RTF is a `\bin9999999999` declaration Tika
# chokes on with EOFException -- Tika's RTF parser is otherwise lenient).
#
# Like #171/#258 these are multipart POSTs in `solr-ref/manifest-multipart.tsv`.
# Separate container/port/core so existing fixtures never re-capture as a side
# effect: re-running this block rewrites only the `extract_{docx,pptx,xlsx,ods,
# odt,odp,rtf}_*` and `extract_broken_*` fixtures it owns.
EXTRACT260_CONTAINER=wayfinder-solr-260
EXTRACT260_SOLR=http://localhost:9030/solr
EXTRACT260_CORE=extract260
docker rm -f "$EXTRACT260_CONTAINER" >/dev/null 2>&1 || true
docker run -d --name "$EXTRACT260_CONTAINER" -p 9030:8983 \
  -e SOLR_MODULES=extraction solr:9.10.1 solr-precreate "$EXTRACT260_CORE" >/dev/null
extract260_ready=false
for _ in $(seq 90); do
  if curl -sf "$EXTRACT260_SOLR/$EXTRACT260_CORE/admin/ping?wt=json" >/dev/null 2>&1; then
    extract260_ready=true
    break
  fi
  sleep 1
done
if [ "$extract260_ready" != true ]; then
  echo "extract260 Solr did not become ready" >&2
  exit 1
fi
curl -sSf "$EXTRACT260_SOLR/$EXTRACT260_CORE/config" -H 'Content-Type: application/json' -d '{
  "add-requesthandler": {
    "name":"/update/extract",
    "class":"solr.extraction.ExtractingRequestHandler",
    "startup":"lazy",
    "defaults": {
      "lowernames":"true", "uprefix":"ignored_", "captureAttr":"true",
      "fmap.a":"links", "fmap.div":"ignored_"
    }
  }
}' >/dev/null
cap_extract260() { # cap_extract260 <name> <expected-status> <query> <input> [mime]
  local name=$1 expected=$2 query=$3 input=$4 mime=${5:-application/octet-stream} actual
  want "$name" || return 0
  actual=$(curl -sS -X POST "$EXTRACT260_SOLR/$EXTRACT260_CORE/update/extract?$query" \
    -F "file=@$HERE/extract-inputs/$input;type=$mime;filename=$input" \
    -o "$OUT/$name.json" -w '%{http_code}')
  if [ "$actual" != "$expected" ]; then
    echo "$name: expected HTTP $expected, got $actual" >&2
    exit 1
  fi
}
# OOXML text family: DOCX, PPTX (bounded zip + streaming xml in Wayfinder).
cap_extract260 extract_docx_xml 200 \
  'extractOnly=true&resource.name=sample.docx&wt=json' sample.docx \
  application/vnd.openxmlformats-officedocument.wordprocessingml.document
cap_extract260 extract_docx_text 200 \
  'extractOnly=true&extractFormat=text&resource.name=sample.docx&wt=json' sample.docx \
  application/vnd.openxmlformats-officedocument.wordprocessingml.document
cap_extract260 extract_pptx_xml 200 \
  'extractOnly=true&resource.name=sample.pptx&wt=json' sample.pptx \
  application/vnd.openxmlformats-officedocument.presentationml.presentation
cap_extract260 extract_pptx_text 200 \
  'extractOnly=true&extractFormat=text&resource.name=sample.pptx&wt=json' sample.pptx \
  application/vnd.openxmlformats-officedocument.presentationml.presentation
# Spreadsheet family: XLSX, ODS (calamine in Wayfinder).
cap_extract260 extract_xlsx_xml 200 \
  'extractOnly=true&resource.name=sample.xlsx&wt=json' sample.xlsx \
  application/vnd.openxmlformats-officedocument.spreadsheetml.sheet
cap_extract260 extract_xlsx_text 200 \
  'extractOnly=true&extractFormat=text&resource.name=sample.xlsx&wt=json' sample.xlsx \
  application/vnd.openxmlformats-officedocument.spreadsheetml.sheet
cap_extract260 extract_ods_xml 200 \
  'extractOnly=true&resource.name=sample.ods&wt=json' sample.ods \
  application/vnd.oasis.opendocument.spreadsheet
cap_extract260 extract_ods_text 200 \
  'extractOnly=true&extractFormat=text&resource.name=sample.ods&wt=json' sample.ods \
  application/vnd.oasis.opendocument.spreadsheet
# ODF text family: ODT, ODP (bounded zip + streaming xml in Wayfinder).
cap_extract260 extract_odt_xml 200 \
  'extractOnly=true&resource.name=sample.odt&wt=json' sample.odt \
  application/vnd.oasis.opendocument.text
cap_extract260 extract_odt_text 200 \
  'extractOnly=true&extractFormat=text&resource.name=sample.odt&wt=json' sample.odt \
  application/vnd.oasis.opendocument.text
cap_extract260 extract_odp_xml 200 \
  'extractOnly=true&resource.name=sample.odp&wt=json' sample.odp \
  application/vnd.oasis.opendocument.presentation
cap_extract260 extract_odp_text 200 \
  'extractOnly=true&extractFormat=text&resource.name=sample.odp&wt=json' sample.odp \
  application/vnd.oasis.opendocument.presentation
# RTF (rtf-parser in Wayfinder).
cap_extract260 extract_rtf_xml 200 \
  'extractOnly=true&resource.name=sample.rtf&wt=json' sample.rtf application/rtf
cap_extract260 extract_rtf_text 200 \
  'extractOnly=true&extractFormat=text&resource.name=sample.rtf&wt=json' sample.rtf application/rtf
# Malformed inputs -> captured 500 envelopes (one per format).
for ext in docx pptx xlsx ods odt odp rtf; do
  cap_extract260 "extract_broken_${ext}" 500 \
    "extractOnly=true&extractFormat=text&resource.name=broken.${ext}&wt=json" "broken.${ext}"
done
echo "captured issue #260 office/ODF/RTF extractOnly fixtures from '$EXTRACT260_CONTAINER' (port 9030)"
docker rm -f "$EXTRACT260_CONTAINER" >/dev/null

# --- /update/extract extractOnly: json.nl named-list shapes (issue #274) -----
#
# #258's follow-up 3: `EXTRACT_PARAMS` allowlists `json.nl` (consistent with
# the other routes), but the handler rendered `file_metadata` in the flat
# alternating array regardless of it. Capturing here to settle, per the
# compatibility contract, what real Solr does with each `json.nl` value on
# `extractOnly` -- so the fixture decides implement-vs-drop rather than the
# issue guessing.
#
# Result: Solr honours `json.nl` on the extract response. `file_metadata` is a
# plain (not `SimpleOrderedMap`) NamedList, so it reshapes per the param --
# `flat` (default) -> `["key",[values],...]`, `map` -> `{"key":[values],...}`,
# `arrarr` -> `[["key",[values]],...]`, `arrmap` -> `[{"key":[values]},...]`.
# `responseHeader` is a `SimpleOrderedMap` and stays an object in every shape,
# and `file` is a String value (not a nested NamedList), so neither moves.
# The `flat` baseline is already `extract_plain_text_xml.json` (#171); this
# block captures the three non-flat shapes on the same plain-text input so the
# only varying factor is `json.nl` itself.
#
# Same ExtractingRequestHandler config as #258/#260 (lowernames/uprefix/
# captureAttr/fmap.a/fmap.div), plain-text input, default (xml/XHTML)
# extractFormat. Separate container/core/port for the same reason #258 used
# one. Like the other extract blocks these are multipart POSTs and live in
# `solr-ref/manifest-multipart.tsv`.
#
# Side note (not captured): an *invalid* `json.nl` value (e.g. `json.nl=garbage`)
# makes Solr's JSONWriter emit truncated, invalid JSON (`"file_metadata"` with
# no value) while still answering HTTP 200. Wayfinder deliberately does not
# reproduce malformed JSON -- unknown values fall back to `flat` via
# `facet::JsonNl::from_params`, consistent with the facet routes. That is a
# defensible divergence from actively-worse captured behaviour (PRD section 2),
# not a to-do, and is not captured as a fixture because the malformed body
# cannot be parsed by the differential harness.
EXTRACT274_CONTAINER=wayfinder-solr-274
EXTRACT274_SOLR=http://localhost:9040/solr
EXTRACT274_CORE=extract274
docker rm -f "$EXTRACT274_CONTAINER" >/dev/null 2>&1 || true
docker run -d --name "$EXTRACT274_CONTAINER" -p 9040:8983 \
  -e SOLR_MODULES=extraction solr:9.10.1 solr-precreate "$EXTRACT274_CORE" >/dev/null
extract274_ready=false
for _ in $(seq 90); do
  if curl -sf "$EXTRACT274_SOLR/$EXTRACT274_CORE/admin/ping?wt=json" >/dev/null 2>&1; then
    extract274_ready=true
    break
  fi
  sleep 1
done
if [ "$extract274_ready" != true ]; then
  echo "extract274 Solr did not become ready" >&2
  exit 1
fi
curl -sSf "$EXTRACT274_SOLR/$EXTRACT274_CORE/config" -H 'Content-Type: application/json' -d '{
  "add-requesthandler": {
    "name":"/update/extract",
    "class":"solr.extraction.ExtractingRequestHandler",
    "startup":"lazy",
    "defaults": {
      "lowernames":"true", "uprefix":"ignored_", "captureAttr":"true",
      "fmap.a":"links", "fmap.div":"ignored_"
    }
  }
}' >/dev/null

cap_extract274() { # cap_extract274 <name> <query-without-json.nl> <json.nl>
  local name=$1 base=$2 nl=$3 actual
  want "$name" || return 0
  actual=$(curl -sS -X POST \
    "$EXTRACT274_SOLR/$EXTRACT274_CORE/update/extract?${base}&json.nl=${nl}" \
    -F "file=@$HERE/extract-inputs/sample.txt;type=application/octet-stream;filename=sample.txt" \
    -o "$OUT/$name.json" -w '%{http_code}')
  if [ "$actual" != 200 ]; then
    echo "$name: expected HTTP 200, got $actual" >&2
    exit 1
  fi
}

BASE274='extractOnly=true&resource.name=sample.txt&wt=json'
cap_extract274 extract_plain_text_json_nl_map    "$BASE274" map
cap_extract274 extract_plain_text_json_nl_arrarr "$BASE274" arrarr
cap_extract274 extract_plain_text_json_nl_arrmap "$BASE274" arrmap
echo "captured issue #274 json.nl extractOnly fixtures from '$EXTRACT274_CONTAINER' (port 9040)"
docker rm -f "$EXTRACT274_CONTAINER" >/dev/null

# --- wave 1 capture prep: {!ex}/{!tag} facet exclusion (#295) ---------------
# One shared block for the two wave-1 issues that need fixtures, so that two
# branches do not each re-run the script and each append to this file. Both sets
# are core-relative GETs against `content`, so both land in `manifest.tsv`.
#
# Corpus reminder (the POST near the top of this script): category counts are
# animals 2 (doc1, doc4), classic 2 (doc1, doc3), garden 1 (doc2), misc 1 (doc3),
# and doc5 has no category at all. `fq=category:animals` therefore narrows the
# unfiltered {animals 2, classic 2, garden 1, misc 1} to {animals 2, classic 1},
# which is what makes an exclusion observable: an excluded facet must still show
# the wider set.
#
# `{!tag=x}` on an `fq` names that filter; `{!ex=x}` on a `facet.field` computes
# that facet as if the named filter were absent. Wayfinder rejects both today
# (`src/local_params.rs`), so every row here is an EXPECTED_DIVERGENCES entry in
# `tests/differential.rs` until #295 lands.
#
# Local params are percent-encoded (`%7B%21tag%3Dcat%7D`, `%20` for the space
# between two of them), matching the `facet_local_params_key` rows above: the
# differential harness GETs each `manifest.tsv` path verbatim, so a raw brace in
# a row would not survive the replay.

# The pair that defines the feature: identical but for the tag/ex.
cap facet_extag_baseline        'select?q=*:*&rows=0&fq=category:animals&facet=true&facet.field=category&wt=json'
cap facet_extag_excluded        'select?q=*:*&rows=0&fq=%7B%21tag%3Dcat%7Dcategory:animals&facet=true&facet.field=%7B%21ex%3Dcat%7Dcategory&wt=json'

# Half-applied: each of tag and ex alone must be a no-op on the counts.
cap facet_extag_tag_no_ex       'select?q=*:*&rows=0&fq=%7B%21tag%3Dcat%7Dcategory:animals&facet=true&facet.field=category&wt=json'
cap facet_extag_ex_no_tag       'select?q=*:*&rows=0&fq=category:animals&facet=true&facet.field=%7B%21ex%3Dcat%7Dcategory&wt=json'
cap facet_extag_ex_unknown_tag  'select?q=*:*&rows=0&fq=%7B%21tag%3Dcat%7Dcategory:animals&facet=true&facet.field=%7B%21ex%3Dnosuch%7Dcategory&wt=json'

# Which filters get excluded when there are several.
cap facet_extag_two_fq_one_tagged 'select?q=*:*&rows=0&fq=%7B%21tag%3Dcat%7Dcategory:animals&fq=category:classic&facet=true&facet.field=%7B%21ex%3Dcat%7Dcategory&wt=json'
cap facet_extag_ex_two_tags     'select?q=*:*&rows=0&fq=%7B%21tag%3Da%7Dcategory:animals&fq=%7B%21tag%3Db%7Dcategory:classic&facet=true&facet.field=%7B%21ex%3Da,b%7Dcategory&wt=json'
cap facet_extag_ex_one_of_two   'select?q=*:*&rows=0&fq=%7B%21tag%3Da%7Dcategory:animals&fq=%7B%21tag%3Db%7Dcategory:classic&facet=true&facet.field=%7B%21ex%3Da%7Dcategory&wt=json'
cap facet_extag_multi_tag       'select?q=*:*&rows=0&fq=%7B%21tag%3Da,b%7Dcategory:animals&facet=true&facet.field=%7B%21ex%3Db%7Dcategory&wt=json'

# Interaction with `{!key}`, which Wayfinder already supports (#138). Both
# orderings, because local-param order is not obviously irrelevant.
cap facet_extag_ex_with_key     'select?q=*:*&rows=0&fq=%7B%21tag%3Dcat%7Dcategory:animals&facet=true&facet.field=%7B%21ex%3Dcat%20key%3Dunfiltered%7Dcategory&wt=json'
cap facet_extag_key_before_ex   'select?q=*:*&rows=0&fq=%7B%21tag%3Dcat%7Dcategory:animals&facet=true&facet.field=%7B%21key%3Dunfiltered%20ex%3Dcat%7Dcategory&wt=json'

# The #299 shape: two facets on one field, one filtered and one not, told apart
# by their keys. This is the row that proves the OR-facet UI is reproducible.
cap facet_extag_both_facets     'select?q=*:*&rows=0&fq=%7B%21tag%3Dcat%7Dcategory:animals&facet=true&facet.field=%7B%21key%3Dfiltered%7Dcategory&facet.field=%7B%21ex%3Dcat%20key%3Dunfiltered%7Dcategory&wt=json'

# Does exclusion reach the other facet types, and interact with facet.* settings?
cap facet_extag_facet_query_ex  'select?q=*:*&rows=0&fq=%7B%21tag%3Dcat%7Dcategory:animals&facet=true&facet.query=%7B%21ex%3Dcat%7Dcategory:classic&wt=json'
cap facet_extag_mincount       'select?q=*:*&rows=0&fq=%7B%21tag%3Dcat%7Dcategory:animals&facet=true&facet.field=%7B%21ex%3Dcat%7Dcategory&facet.mincount=2&wt=json'
cap facet_extag_missing        'select?q=*:*&rows=0&fq=%7B%21tag%3Dcat%7Dcategory:animals&facet=true&facet.field=%7B%21ex%3Dcat%7Dcategory&facet.missing=true&wt=json'

# Degenerate forms: is a tag on `q` meaningful, and are empty names an error?
cap facet_extag_tag_on_q       'select?q=%7B%21tag%3Dcat%7D*:*&rows=0&facet=true&facet.field=%7B%21ex%3Dcat%7Dcategory&wt=json'
cap facet_extag_ex_empty       'select?q=*:*&rows=0&fq=%7B%21tag%3Dcat%7Dcategory:animals&facet=true&facet.field=%7B%21ex%3D%7Dcategory&wt=json'
cap facet_extag_tag_empty      'select?q=*:*&rows=0&fq=%7B%21tag%3D%7Dcategory:animals&facet=true&facet.field=%7B%21ex%3Dcat%7Dcategory&wt=json'

# --- wave 1 capture prep: terms.prefix and terms.limit (#308) --------------
# `search_api_autocomplete` sends `terms.fl` + `terms.prefix` + `terms.limit` on
# every keystroke (finding 131). Only `terms.fl` is in `TERMS_PARAMS`, so these
# rows are also EXPECTED_DIVERGENCES entries until #308 lands.
#
# `body` is text_en, so its index terms are stemmed and stopped: doc bodies
# reduce to afternoon, all, brown, cat, dai, dog, fox, garden, jump, lazi, live,
# much, noth, quick, save, think, togeth. That matters for choosing prefixes --
# `terms.prefix=th` hits the stem `think`, not the surface form "thinking", and
# `terms.prefix=a` hits `afternoon` and `all`, which both have count 1 and so pin
# the count-sort tie-break. `category` is a string field with no analysis, for
# the unanalyzed comparison.

cap terms_prefix_body_multi    'terms?terms=true&terms.fl=body&terms.prefix=d&omitHeader=true&wt=json'
cap terms_prefix_body_single   'terms?terms=true&terms.fl=body&terms.prefix=th&omitHeader=true&wt=json'
cap terms_prefix_body_none     'terms?terms=true&terms.fl=body&terms.prefix=zzz&omitHeader=true&wt=json'
cap terms_prefix_tie           'terms?terms=true&terms.fl=body&terms.prefix=a&omitHeader=true&wt=json'
cap terms_prefix_string_field  'terms?terms=true&terms.fl=category&terms.prefix=c&omitHeader=true&wt=json'
cap terms_prefix_empty         'terms?terms=true&terms.fl=body&terms.prefix=&omitHeader=true&wt=json'
cap terms_prefix_case          'terms?terms=true&terms.fl=body&terms.prefix=D&omitHeader=true&wt=json'
cap terms_prefix_two_fields    'terms?terms=true&terms.fl=body&terms.fl=category&terms.prefix=a&omitHeader=true&wt=json'
cap terms_prefix_unknown_field 'terms?terms=true&terms.fl=nosuchfield&terms.prefix=a&omitHeader=true&wt=json'

cap terms_limit_below          'terms?terms=true&terms.fl=body&terms.prefix=d&terms.limit=1&omitHeader=true&wt=json'
cap terms_limit_above          'terms?terms=true&terms.fl=body&terms.prefix=d&terms.limit=99&omitHeader=true&wt=json'
cap terms_limit_zero           'terms?terms=true&terms.fl=body&terms.prefix=d&terms.limit=0&omitHeader=true&wt=json'
cap terms_limit_negative       'terms?terms=true&terms.fl=body&terms.limit=-1&omitHeader=true&wt=json'
cap terms_limit_no_prefix      'terms?terms=true&terms.fl=body&terms.limit=2&omitHeader=true&wt=json'
cap terms_limit_invalid        'terms?terms=true&terms.fl=body&terms.limit=abc&omitHeader=true&wt=json'
cap terms_prefix_json_nl_map   'terms?terms=true&terms.fl=body&terms.prefix=d&json.nl=map&omitHeader=true&wt=json'

# --- result grouping (issue #290, finding 130) ----------------------------
# `search_api_solr`'s `setGrouping()` (`SearchApiSolrBackend.php:4575-4634`)
# sends `group=true` plus six `group.*` params: `group.field` (repeatable),
# `group.ngroups=true` (unconditional), `group.truncate`, `group.facet`,
# `group.limit` (when set & != 1), `group.offset` (when set), and `group.sort`
# (a single comma-joined string). `group.format`/`group.main` are NEVER sent
# (finding 130), so they are out of scope and deliberately absent from
# `SELECT_PARAMS` (they 400 under strict_params, as they should). The module
# refuses to group on a fulltext or multiValued field -- and so does Solr
# itself (`can not use FieldCache on multivalued field`), so the server side
# only needs single-valued non-text fields.
#
# Own container, own port (wayfinder-solr-290, 8997), own core `grouping`,
# per the stats/highlight precedent: the canonical `content` corpus has no
# single-valued field with repeated values (`id` is unique, `category` is
# multiValued), so meaningful multi-doc groups need their own schema+corpus.
# Manifest-rows are core-qualified (`grouping/select?...`) like the stats rows.
GROUPING_CONTAINER=wayfinder-solr-290
GROUPING_SOLR=http://localhost:8997/solr
GROUPING_CORE=grouping
if ! docker ps --format '{{.Names}}' | grep -qx "$GROUPING_CONTAINER"; then
  docker rm -f "$GROUPING_CONTAINER" >/dev/null 2>&1 || true
  docker run -d --name "$GROUPING_CONTAINER" -p 8997:8983 \
    solr:9 solr-precreate "$GROUPING_CORE" >/dev/null
fi
echo -n "waiting for grouping solr"
for _ in $(seq 60); do
  if curl -sf "$GROUPING_SOLR/$GROUPING_CORE/admin/ping?wt=json" >/dev/null 2>&1; then echo " ok"; break; fi
  echo -n "."; sleep 1
done
curl -s "$GROUPING_SOLR/$GROUPING_CORE/schema" -H 'Content-Type: application/json' -d '{
  "add-field": [
    {"name":"body",       "type":"text_en", "indexed":true, "stored":true},
    {"name":"type",       "type":"string",  "indexed":true, "stored":true, "docValues":true},
    {"name":"category",   "type":"string",  "indexed":true, "stored":true, "docValues":true, "multiValued":true},
    {"name":"popularity", "type":"pint",    "indexed":true, "stored":true, "docValues":true}
  ]
}' >/dev/null
# Six docs chosen so `type` has two multi-doc groups plus a null group:
# article={g1,g3,g4} (3), page={g2,g5} (2), null={g6} (1). ngroups=3, matches=6.
# `category` is multiValued (for the multivalued-grouping 400), `popularity`
# is single-valued numeric (for grouping on a numeric field), `body` backs
# scored queries.
curl -sf "$GROUPING_SOLR/$GROUPING_CORE/update?commit=true" -H 'Content-Type: application/json' -d '[
  {"id":"g1","type":"article","category":["news"],"body":"lazy dog brown","popularity":10},
  {"id":"g2","type":"page","category":["news"],"body":"lazy garden afternoon","popularity":20},
  {"id":"g3","type":"article","category":["blog"],"body":"quick thinking saves","popularity":30},
  {"id":"g4","type":"article","category":["blog"],"body":"dogs cats together","popularity":5},
  {"id":"g5","type":"page","body":"nothing here","popularity":40},
  {"id":"g6","body":"orphan ungrouped","popularity":15}
]' >/dev/null

capg() {  # capg <name> <url-after-/solr/>, against $GROUPING_SOLR
  local name=$1 suffix=$2
  want "$name" || return 0
  curl -sg "$GROUPING_SOLR/$suffix" -o "$OUT/$name.json" -w '%{http_code}' > "$OUT/$name.status"
  printf '%s\t%s\t%s\t%s\t%s\t%s\n' \
    "$name" "$(cat "$OUT/$name.status")" GET "$suffix" "" "$GROUPING_SOLR" \
    >> "$MANIFEST_ERRORS"
  rm -f "$OUT/$name.status"
}

# Every happy path uses fl=id (no _version_/_root_), and q=*:* (every doc
# scores 1.0, so there is no BM25 score-magnitude variance for the
# differential harness to tolerate -- scored/relevance group ORDERING is
# tested in tests/grouping.rs, not pinned by these fixtures).
capg group_basic         'grouping/select?q=*:*&group=true&group.field=type&group.ngroups=true&fl=id&wt=json'
capg group_ngroups_off   'grouping/select?q=*:*&group=true&group.field=type&fl=id&wt=json'
capg group_limit         'grouping/select?q=*:*&group=true&group.field=type&group.limit=2&group.ngroups=true&fl=id&wt=json'
capg group_offset        'grouping/select?q=*:*&group=true&group.field=type&group.offset=1&group.ngroups=true&fl=id&wt=json'
capg group_rows_start    'grouping/select?q=*:*&group=true&group.field=type&rows=2&start=1&group.ngroups=true&fl=id&wt=json'
capg group_sort          'grouping/select?q=*:*&group=true&group.field=type&group.sort=id+desc&group.limit=2&group.ngroups=true&fl=id&wt=json'
capg group_multi_field   'grouping/select?q=*:*&group=true&group.field=type&group.field=id&group.ngroups=true&fl=id&wt=json'
capg group_numeric       'grouping/select?q=*:*&group=true&group.field=popularity&group.ngroups=true&fl=id&wt=json'
capg group_fq            'grouping/select?q=*:*&fq=type:article&group=true&group.field=type&group.ngroups=true&fl=id&wt=json'
capg group_fl_score      'grouping/select?q=*:*&group=true&group.field=type&group.ngroups=true&fl=id,score&wt=json'
capg group_zero          'grouping/select?q=zzznomatch&df=body&group=true&group.field=type&group.ngroups=true&fl=id&wt=json'

# Error shapes (Solr 400s). The differential harness normalises error.msg and
# error.metadata away, so only status 400 / error.code 400 is compared here;
# tests/grouping.rs pins the message text.
capg group_err_no_field      'grouping/select?q=*:*&group=true&fl=id&wt=json'
capg group_err_unknown_field 'grouping/select?q=*:*&group=true&group.field=nosuchfield&fl=id&wt=json'
capg group_err_multivalued   'grouping/select?q=*:*&group=true&group.field=category&fl=id&wt=json'

release "$GROUPING_CONTAINER" "grouping core '$GROUPING_CORE'"

# --- function queries: {!func} and {!boost b=...} (issue #289) -------------
# #289 (finding 129): search_api_solr's document-boost path emits the score
# inline in `q` as `{!boost b=sum(boost_document,...)}` or `{!boost
# b=boost_document}` (SearchApiSolrBackend.php:1953-1977) -- never as `bf=`.
# That makes the function-query *evaluator* the real dependency, reached
# through the `{!func}`/`{!boost}` query-parser local params, not a fixed
# function list reached through `bf`. This block captures the wire shape and
# exact scores of the arithmetic subset: constants, numeric field references,
# and the functions `sum`/`max`/`product`/`recip`. (`payload_score` is a
# *separate* query parser -- `{!payload_score f=boost_term v=... func=max}`
# over a payload-bearing field type -- emitted by
# Utility::flattenKeysToPayloadScore, which is outside the three-file
# snapshot; verified against the 4.4.x source at git.drupalcode.org. It needs
# its own `boost_term_payload` field type and is a follow-up increment, not
# part of this arithmetic evaluator. `ms`/`rord` are off the corrected client
# path -- BoostMoreRecent does not emit `product(...,recip(ms(...)))` as `bf`,
# finding 129 corrected that premise -- and need date/ordinal field types, so
# they are out of scope here too.)
#
# Every score fixture uses `q=*:*` (or `{!func}` alone): a `*:*` match scores
# a constant 1.0 in Solr, so `{!boost b=<f>}*:*` is `1.0 * <f>` and `bf=<f>`
# is `1.0 + <f>` -- the captured score is the pure function value, with no
# BM25 base. That keeps the differential comparison exact rather than under
# the BM25-magnitude ratified divergence (PRD div. 4), and it is the reason
# the `bf`/`boost` rows below use `q=*:*` rather than a text query.
#
# Own container/port/core for the same reason every other appended block uses
# one: a function-query corpus needs numeric `docValues` fields, which the
# base `content` core does not have. Rows land in `manifest-errors.tsv` (not
# `manifest.tsv`) and get a dedicated `fnq_app` in `tests/differential.rs`,
# like `sortdebt`/`facets33`. d4 has no `price` and d5 has no `views` so a
# missing numeric value resolves to 0, the Solr function-query default.
FNQ_CONTAINER=wayfinder-solr-289
FNQ_SOLR=http://localhost:9060/solr
FNQ_CORE=fnq
if want_any '^fnq_'; then
  if ! docker ps --format '{{.Names}}' | grep -qx "$FNQ_CONTAINER"; then
    docker rm -f "$FNQ_CONTAINER" >/dev/null 2>&1 || true
    docker run -d --name "$FNQ_CONTAINER" -p 9060:8983 \
      solr:9 solr-precreate "$FNQ_CORE" >/dev/null
  fi
  echo -n "waiting for fnq solr"
  for _ in $(seq 60); do
    if curl -sf "$FNQ_SOLR/$FNQ_CORE/admin/ping?wt=json" >/dev/null 2>&1; then echo " ok"; break; fi
    echo -n "."; sleep 1
  done
  curl -s "$FNQ_SOLR/$FNQ_CORE/schema" -H 'Content-Type: application/json' -d '{
    "add-field": [
      {"name":"body","type":"text_general","indexed":true,"stored":true},
      {"name":"boost_document","type":"pfloat","indexed":true,"stored":true,"docValues":true},
      {"name":"views","type":"pint","indexed":true,"stored":true,"docValues":true},
      {"name":"rating","type":"pfloat","indexed":true,"stored":true,"docValues":true},
      {"name":"price","type":"pdouble","indexed":true,"stored":true,"docValues":true}
    ]
  }' >/dev/null
  curl -sf "$FNQ_SOLR/$FNQ_CORE/update?commit=true" -H 'Content-Type: application/json' -d '[
    {"id":"d1","body":"quick brown fox","boost_document":1.0,"views":10,"rating":2.0,"price":5.0},
    {"id":"d2","body":"lazy dog","boost_document":3.0,"views":30,"rating":4.0,"price":15.0},
    {"id":"d3","body":"quick dog","boost_document":2.0,"views":20,"rating":6.0,"price":10.0},
    {"id":"d4","body":"quick fox","boost_document":0.5,"views":40,"rating":1.0},
    {"id":"d5","body":"lazy brown","boost_document":2.5,"rating":5.0,"price":8.0}
  ]' >/dev/null
fi

capf289() {  # capf289 <name> <path-after-core>
  local name=$1 suffix=$2
  want "$name" || return 0
  curl -sg "$FNQ_SOLR/$FNQ_CORE/$suffix" \
    -o "$OUT/$name.json" -w '%{http_code}' > "$OUT/$name.status"
  printf '%s\t%s\t%s\t%s\t%s\t%s\n' \
    "$name" "$(cat "$OUT/$name.status")" GET "$FNQ_CORE/$suffix" "" "$FNQ_SOLR" \
    >> "$MANIFEST_ERRORS"
  rm -f "$OUT/$name.status"
}

# All suffixes are percent-encoded: `{`/`}`/`!`/`(`/`)`/space/inner `=` would
# otherwise break the in-process axum URI the differential harness replays
# against (tests/common/mod.rs builds `.uri("/solr/...")` from this column),
# and `Params::parse` percent-decodes them back. curl sends the `%XX` form
# verbatim and real Solr decodes it too, so capture and replay see the same
# query. This matches the encoded convention every `{!...}` manifest row uses
# (see `facet_extag_*`).
#
# {!func} query parser: ranks every doc by the function value (score = value).
capf289 fnq_func_field         'select?q=%7B%21func%7Dboost_document&fl=id,score&wt=json'
capf289 fnq_func_sum           'select?q=%7B%21func%7Dsum%28boost_document,rating%29&fl=id,score&wt=json'
capf289 fnq_func_max           'select?q=%7B%21func%7Dmax%28rating,price%29&fl=id,score&wt=json'
capf289 fnq_func_product       'select?q=%7B%21func%7Dproduct%28rating,2%29&fl=id,score&wt=json'
capf289 fnq_func_recip         'select?q=%7B%21func%7Drecip%28rating,1,1,1%29&fl=id,score&wt=json'
capf289 fnq_func_const         'select?q=%7B%21func%7Dsum%281,2,3%29&fl=id,score&sort=id%20asc&wt=json'
capf289 fnq_func_missing       'select?q=%7B%21func%7Dsum%28views,rating%29&fl=id,score&wt=json'

# {!boost b=<func>} query parser: multiplies the wrapped query's score by the
# function value. With *:* (constant 1.0) the score is the function value.
capf289 fnq_boost_field        'select?q=%7B%21boost%20b%3Dboost_document%7D*:*&fl=id,score&wt=json'
capf289 fnq_boost_sum          'select?q=%7B%21boost%20b%3Dsum%28boost_document,rating%29%7D*:*&fl=id,score&wt=json'

# edismax bf (additive) and boost (multiplicative) params with a function
# value. *:* keeps the base score constant at 1.0 so the captured score is a
# clean 1.0+value / 1.0*value. These two rows are why #232's bf/boost warnings
# come off once this lands: a function-form value is now applied, not ignored.
capf289 fnq_bf_additive        'select?q=*:*&defType=edismax&qf=body&bf=sum%28views,rating%29&fl=id,score&wt=json'
capf289 fnq_boost_param        'select?q=*:*&defType=edismax&qf=body&boost=product%28rating,2%29&fl=id,score&wt=json'

# Error shapes (400): unknown function, unbalanced parens, empty body, and
# an unknown field reference. error.msg/metadata/trace are normalised away by
# the differential harness; only status + error.code are compared.
capf289 fnq_err_unknown_func   'select?q=%7B%21func%7Dbogus%281,2%29&fl=id,score&wt=json'
capf289 fnq_err_unbalanced     'select?q=%7B%21func%7Dsum%28boost_document&fl=id,score&wt=json'
capf289 fnq_err_empty          'select?q=%7B%21func%7D&fl=id,score&wt=json'
capf289 fnq_err_unknown_field  'select?q=%7B%21func%7Dnosuchfield&fl=id,score&wt=json'

if want_any '^fnq_'; then
  release "$FNQ_CONTAINER" "function-query core '$FNQ_CORE'"
fi

# --- per-field facet settings: f.<X>.facet.* and facet.* as local params (#296)
# The premise this block exists to settle. `search_api_solr` emits per-facet
# settings through Solarium as `f.<local_key>.facet.limit|mincount|missing`, and
# its `local_key` is always the *Solr field name* (SearchApiSolrBackend::setFacets
# discards the Search API delta and calls createFacetField($solr_field)). Since
# #299 *this* module keys facets by the delta, so key and field name can differ,
# and nothing in the captured contract says which of the two `f.<X>.facet.*`
# resolves against. Wayfinder implements exactly one per-field setting today --
# `f.<field>.facet.missing`, issue #140, keyed by field name (src/facet.rs:527) --
# so the field-name reading is the presumption, not a finding.
#
# Rows are core-relative GETs against `content`, so they land in `manifest.tsv`
# alongside the `facet_extag_*` block above, and reuse its corpus: category is
# animals 2 (doc1, doc4), classic 2 (doc1, doc3), garden 1 (doc2), misc 1 (doc3),
# with doc5 carrying no category at all. `id` is the second facet field
# throughout -- five buckets of 1 -- and is there to show whether an
# `f.category.facet.*` override leaks onto a field it does not name.
#
# Local params are percent-encoded for the same reason as the block above: the
# differential harness GETs each manifest path verbatim.

# A. Per-field overrides keyed by field name, no {!key} in play. If these do not
# work the whole feature is misconceived, so they come first.
cap facet_perfield_limit            'select?q=*:*&rows=0&facet=true&facet.field=category&facet.field=id&f.category.facet.limit=1&wt=json'
cap facet_perfield_mincount         'select?q=*:*&rows=0&facet=true&facet.field=category&facet.field=id&f.category.facet.mincount=2&wt=json'
cap facet_perfield_sort_index       'select?q=*:*&rows=0&facet=true&facet.field=category&f.category.facet.sort=index&wt=json'
cap facet_perfield_overrides_global 'select?q=*:*&rows=0&facet=true&facet.field=category&facet.field=id&facet.limit=1&f.category.facet.limit=-1&wt=json'
cap facet_perfield_unknown_field    'select?q=*:*&rows=0&facet=true&facet.field=category&f.nosuchfield.facet.limit=1&wt=json'

# B. The decisive pair: one keyed facet, and the override addressed once by the
# field name and once by the key. Whichever row shows a limited list is the
# answer; both showing it means either address works.
cap facet_perfield_key_by_field     'select?q=*:*&rows=0&facet=true&facet.field=%7B%21key%3Dcat%7Dcategory&f.category.facet.limit=1&wt=json'
cap facet_perfield_key_by_key       'select?q=*:*&rows=0&facet=true&facet.field=%7B%21key%3Dcat%7Dcategory&f.cat.facet.limit=1&wt=json'

# C. The other candidate mechanism: the setting carried as a local param on the
# facet.field itself. SimpleFacets.parseParams wraps the local params over the
# request params (SolrParams.wrapDefaults), which would make this work for any
# facet.* setting -- and unlike `f.<X>.facet.*` it is unambiguous when two facets
# share a field.
cap facet_perfield_lp_limit         'select?q=*:*&rows=0&facet=true&facet.field=%7B%21key%3Dcat%20facet.limit%3D1%7Dcategory&wt=json'
cap facet_perfield_lp_mincount      'select?q=*:*&rows=0&facet=true&facet.field=%7B%21key%3Dcat%20facet.mincount%3D2%7Dcategory&wt=json'
cap facet_perfield_lp_sort          'select?q=*:*&rows=0&facet=true&facet.field=%7B%21key%3Dcat%20facet.sort%3Dindex%7Dcategory&wt=json'
cap facet_perfield_lp_missing       'select?q=*:*&rows=0&facet=true&facet.field=%7B%21key%3Dcat%20facet.missing%3Dtrue%7Dcategory&wt=json'
cap facet_perfield_lp_no_key        'select?q=*:*&rows=0&facet=true&facet.field=%7B%21facet.limit%3D1%7Dcategory&wt=json'

# D. Two facets on one field with *different* settings -- the case #296 exists
# for, and the one `f.<field>.facet.*` cannot express if it resolves by field
# name. Captured both ways.
cap facet_perfield_two_lp           'select?q=*:*&rows=0&facet=true&facet.field=%7B%21key%3Da%20facet.limit%3D1%7Dcategory&facet.field=%7B%21key%3Db%20facet.limit%3D3%7Dcategory&wt=json'
cap facet_perfield_two_by_key       'select?q=*:*&rows=0&facet=true&facet.field=%7B%21key%3Da%7Dcategory&facet.field=%7B%21key%3Db%7Dcategory&f.a.facet.limit=1&f.b.facet.limit=3&wt=json'

# E. Against an excluded (OR) facet. Finding 140 pins facet.mincount and
# facet.missing as post-exclusion; it says nothing about facet.limit, and limit
# applied pre-exclusion would silently truncate the wider list an OR facet exists
# to show. The last row is the full search_api_solr OR shape: the filtered facet
# and the excluded one, told apart by key, with a setting on the excluded one.
cap facet_perfield_ex_limit         'select?q=*:*&rows=0&fq=%7B%21tag%3Dcat%7Dcategory:animals&facet=true&facet.field=%7B%21ex%3Dcat%20key%3Dun%7Dcategory&f.category.facet.limit=1&wt=json'
cap facet_perfield_ex_lp_limit      'select?q=*:*&rows=0&fq=%7B%21tag%3Dcat%7Dcategory:animals&facet=true&facet.field=%7B%21ex%3Dcat%20key%3Dun%20facet.limit%3D1%7Dcategory&wt=json'
cap facet_perfield_ex_two_facets    'select?q=*:*&rows=0&fq=%7B%21tag%3Dcat%7Dcategory:animals&facet=true&facet.field=%7B%21key%3Dfiltered%7Dcategory&facet.field=%7B%21ex%3Dcat%20key%3Dun%20facet.limit%3D1%7Dcategory&wt=json'

# F. Error shape: a non-numeric per-field limit. The differential harness
# normalises error.msg/metadata away, so this pins status and error.code only.
cap facet_perfield_err_bad_limit    'select?q=*:*&rows=0&facet=true&facet.field=category&f.category.facet.limit=abc&wt=json'

# The pair above is not decisive on its own: with `fq=category:animals` the top
# bucket is `animals` whether the limit ranks the filtered counts or the excluded
# ones. `fq=category:garden` separates them -- filtered ranking puts `garden`
# first, excluded ranking puts `animals` first -- so these two rows are the ones
# that say whether facet.limit is applied before or after the exclusion.
cap facet_perfield_ex_limit_rank    'select?q=*:*&rows=0&fq=%7B%21tag%3Dcat%7Dcategory:garden&facet=true&facet.field=%7B%21ex%3Dcat%20key%3Dun%7Dcategory&f.category.facet.limit=1&wt=json'
cap facet_perfield_ex_lp_limit_rank 'select?q=*:*&rows=0&fq=%7B%21tag%3Dcat%7Dcategory:garden&facet=true&facet.field=%7B%21ex%3Dcat%20key%3Dun%20facet.limit%3D1%7Dcategory&wt=json'

# --- per-field facet.sort, on a corpus that can show it (#296) --------------
# `facet.sort` is the one setting the `content` corpus cannot pin: its category
# values are animals 2, classic 2, garden 1, misc 1, so count order and index
# order are the same list and every ordering claim would be vacuous. This core
# exists only to break that tie -- `topic` is zebra 3, mango 2, apple 1, so count
# order is zebra, mango, apple and index order is apple, mango, zebra, and a
# limit of 1 tells them apart outright. Field and key names are deliberately
# different strings (`topic` vs `k`) so `f.<X>.facet.sort` cannot accidentally
# address both.
#
# Rows land in `manifest-errors.tsv` (own core, like `fnq_*`/`group_*`) and need
# their own app in `tests/differential.rs` when #296 lands.
PF296_CONTAINER=wayfinder-solr-296
PF296_SOLR=http://localhost:9071/solr
PF296_CORE=pf296
if want_any '^pf296_'; then
  if ! docker ps --format '{{.Names}}' | grep -qx "$PF296_CONTAINER"; then
    docker rm -f "$PF296_CONTAINER" >/dev/null 2>&1 || true
    docker run -d --name "$PF296_CONTAINER" -p 9071:8983 \
      solr:9 solr-precreate "$PF296_CORE" >/dev/null
  fi
  echo -n "waiting for pf296 solr"
  for _ in $(seq 60); do
    if curl -sf "$PF296_SOLR/$PF296_CORE/admin/ping?wt=json" >/dev/null 2>&1; then echo " ok"; break; fi
    echo -n "."; sleep 1
  done
  curl -s "$PF296_SOLR/$PF296_CORE/schema" -H 'Content-Type: application/json' -d '{
    "add-field": [
      {"name":"topic","type":"string","indexed":true,"stored":true,
       "docValues":true,"multiValued":true}
    ]
  }' >/dev/null
  curl -sf "$PF296_SOLR/$PF296_CORE/update?commit=true" -H 'Content-Type: application/json' -d '[
    {"id":"s1","topic":["zebra"]},
    {"id":"s2","topic":["zebra","mango"]},
    {"id":"s3","topic":["zebra","mango","apple"]},
    {"id":"s4"}
  ]' >/dev/null
fi

capp296() {  # capp296 <name> <path-after-core>
  local name=$1 suffix=$2
  want "$name" || return 0
  curl -sg "$PF296_SOLR/$PF296_CORE/$suffix" \
    -o "$OUT/$name.json" -w '%{http_code}' > "$OUT/$name.status"
  printf '%s\t%s\t%s\t%s\t%s\t%s\n' \
    "$name" "$(cat "$OUT/$name.status")" GET "$PF296_CORE/$suffix" "" "$PF296_SOLR" \
    >> "$MANIFEST_ERRORS"
  rm -f "$OUT/$name.status"
}

# Controls: the two orderings, requested globally.
capp296 pf296_sort_global_count 'select?q=*:*&rows=0&facet=true&facet.field=topic&wt=json'
capp296 pf296_sort_global_index 'select?q=*:*&rows=0&facet=true&facet.field=topic&facet.sort=index&wt=json'

# Per-field, by field name -- with and without a {!key}, and against a global
# that says the opposite.
capp296 pf296_sort_field        'select?q=*:*&rows=0&facet=true&facet.field=topic&f.topic.facet.sort=index&wt=json'
capp296 pf296_sort_field_wins   'select?q=*:*&rows=0&facet=true&facet.field=topic&facet.sort=count&f.topic.facet.sort=index&facet.limit=1&wt=json'
capp296 pf296_sort_key_by_field 'select?q=*:*&rows=0&facet=true&facet.field=%7B%21key%3Dk%7Dtopic&f.topic.facet.sort=index&facet.limit=1&wt=json'
capp296 pf296_sort_key_by_key   'select?q=*:*&rows=0&facet=true&facet.field=%7B%21key%3Dk%7Dtopic&f.k.facet.sort=index&facet.limit=1&wt=json'

# Local param, including the two-facets-one-field case that per-field params
# cannot express: one facet ordered by count, the other by index, same field.
capp296 pf296_sort_lp           'select?q=*:*&rows=0&facet=true&facet.field=%7B%21key%3Dk%20facet.sort%3Dindex%7Dtopic&facet.limit=1&wt=json'
capp296 pf296_sort_two_lp       'select?q=*:*&rows=0&facet=true&facet.field=%7B%21key%3Dbycount%20facet.sort%3Dcount%20facet.limit%3D1%7Dtopic&facet.field=%7B%21key%3Dbyindex%20facet.sort%3Dindex%20facet.limit%3D1%7Dtopic&wt=json'

if want_any '^pf296_'; then
  release "$PF296_CONTAINER" "per-field facet-sort core '$PF296_CORE'"
fi

# --- #296 precedence: local param vs f.<field>.facet.* vs global -----------
# Findings 147/148 established that both addresses work; they did not say which
# wins when they disagree, and Solr's own mechanism makes the answer
# non-obvious. `SimpleFacets.parseParams` does
# `SolrParams.wrapDefaults(localParams, orig)` and then reads the setting with
# `getFieldParam(field, "facet.limit")`, which looks for `f.<field>.facet.limit`
# *before* the bare name -- so an `f.<field>.` param in the request could beat a
# local param on the facet itself, which is the opposite of what "local params
# shadow the request" suggests. These three rows decide it: each sets two of the
# three addresses to conflicting limits over `category` (4 buckets).
cap facet_perfield_prec_lp_vs_field  'select?q=*:*&rows=0&facet=true&facet.field=%7B%21key%3Dcat%20facet.limit%3D1%7Dcategory&f.category.facet.limit=3&wt=json'
cap facet_perfield_prec_lp_vs_global 'select?q=*:*&rows=0&facet=true&facet.field=%7B%21key%3Dcat%20facet.limit%3D1%7Dcategory&facet.limit=3&wt=json'
cap facet_perfield_prec_field_vs_global 'select?q=*:*&rows=0&facet=true&facet.field=category&f.category.facet.limit=1&facet.limit=3&wt=json'

# --- function range queries: {!frange l=.. u=..}<func> (issue #333) ---------
# #333 (finding 133): `{!frange}` is Solr's general range-filter-over-function
# (`FunctionRangeQParserPlugin` -> `ValueSourceRangeFilter`), not a geo-specific
# construct -- `geodist()` is simply the function that flows through it on the
# Drupal `setSpatial()` path. Built this way it stays reusable: any function
# the #289 evaluator knows (`rating`, `product(rating,2)`, constants, ...) is a
# valid frange body, and `{!frange}geodist()` composes for free once #332 lands
# the `GeoDist` variant. This block captures frange over the SAME numeric
# docValues corpus the `fnq` (#289) block uses, so the fixtures are directly
# analogous, and it needs no geo / location field at all.
#
# Verified against `solr:9` (FunctionRangeQParserPlugin.java, 9.10.1): the
# local params are `l` (lower, optional), `u` (upper, optional), `incl`
# (include lower bound, boolean, default true) and `incu` (include upper bound,
# boolean, default true) -- NOT `incl`/`excl` with the tokens "lower"/"upper";
# those 400 as `invalid boolean value`. A doc matches iff the function value
# *exists* for that doc (every referenced field has a value) AND falls in the
# (half-open) range. Missing-field docs are therefore EXCLUDED, even though a
# bare `{!func}field` scores them 0 -- frange is an `exists()` filter, func is
# a scorer, and that is the load-bearing difference between the two paths.
#
# Own container/port/core for the same reason `fnq` uses one: a function-range
# corpus needs numeric `docValues` fields, which the base `content` core lacks.
# Rows land in `manifest-errors.tsv` (not `manifest.tsv`) and get a dedicated
# `frange_app` in `tests/differential.rs`, like `fnq`/`facets33`/`pf296`.
FRANGE_CONTAINER=wayfinder-solr-333
FRANGE_SOLR=http://localhost:9072/solr
FRANGE_CORE=frange
if want_any '^frange_'; then
  if ! docker ps --format '{{.Names}}' | grep -qx "$FRANGE_CONTAINER"; then
    docker rm -f "$FRANGE_CONTAINER" >/dev/null 2>&1 || true
    docker run -d --name "$FRANGE_CONTAINER" -p 9072:8983 \
      solr:9 solr-precreate "$FRANGE_CORE" >/dev/null
  fi
  echo -n "waiting for frange solr"
  for _ in $(seq 60); do
    if curl -sf "$FRANGE_SOLR/$FRANGE_CORE/admin/ping?wt=json" >/dev/null 2>&1; then echo " ok"; break; fi
    echo -n "."; sleep 1
  done
  curl -s "$FRANGE_SOLR/$FRANGE_CORE/schema" -H 'Content-Type: application/json' -d '{
    "add-field": [
      {"name":"body","type":"text_general","indexed":true,"stored":true},
      {"name":"boost_document","type":"pfloat","indexed":true,"stored":true,"docValues":true},
      {"name":"views","type":"pint","indexed":true,"stored":true,"docValues":true},
      {"name":"rating","type":"pfloat","indexed":true,"stored":true,"docValues":true},
      {"name":"price","type":"pdouble","indexed":true,"stored":true,"docValues":true}
    ]
  }' >/dev/null
  # Same 5-doc corpus as the fnq block verbatim: d4 has no `price`, d5 has no
  # `views`, and every doc has a `rating` -- so a missing numeric value is
  # observable on `price`/`views` (frange excludes it; func would score it 0).
  curl -sf "$FRANGE_SOLR/$FRANGE_CORE/update?commit=true" -H 'Content-Type: application/json' -d '[
    {"id":"d1","body":"quick brown fox","boost_document":1.0,"views":10,"rating":2.0,"price":5.0},
    {"id":"d2","body":"lazy dog","boost_document":3.0,"views":30,"rating":4.0,"price":15.0},
    {"id":"d3","body":"quick dog","boost_document":2.0,"views":20,"rating":6.0,"price":10.0},
    {"id":"d4","body":"quick fox","boost_document":0.5,"views":40,"rating":1.0},
    {"id":"d5","body":"lazy brown","boost_document":2.5,"rating":5.0,"price":8.0}
  ]' >/dev/null
fi

capfrange() {  # capfrange <name> <path-after-core>
  local name=$1 suffix=$2
  want "$name" || return 0
  curl -sg "$FRANGE_SOLR/$FRANGE_CORE/$suffix" \
    -o "$OUT/$name.json" -w '%{http_code}' > "$OUT/$name.status"
  printf '%s\t%s\t%s\t%s\t%s\t%s\n' \
    "$name" "$(cat "$OUT/$name.status")" GET "$FRANGE_CORE/$suffix" "" "$FRANGE_SOLR" \
    >> "$MANIFEST_ERRORS"
  rm -f "$OUT/$name.status"
}

# Suffixes are percent-encoded for the same reason fnq's are: `{`/`}`/`!`/`(
# /)`/space/inner `=` would break the in-process axum URI the differential
# harness replays (tests/common/mod.rs builds `.uri("/solr/...")` from this
# column), and `Params::parse` percent-decodes them back. See fnq's encoding
# note.
#
# rating corpus: d1=2 d2=4 d3=6 d4=1, d5 has NO rating (excluded by frange).
#
# Bounds, default inclusive both. rating in [2,6] -> d1,d2,d3.
capfrange frange_inclusive        'select?q=*:*&fq=%7B%21frange%20l%3D2%20u%3D6%7Drating&fl=id&sort=id%20asc&wt=json'
# Lower bound only (>=4): d2,d3.
capfrange frange_lower_only       'select?q=*:*&fq=%7B%21frange%20l%3D4%7Drating&fl=id&sort=id%20asc&wt=json'
# Upper bound only (<=2): d1(2),d4(1); d5 has no rating so is EXCLUDED even
# though a missing numeric value would evaluate to 0 (<=2).
capfrange frange_upper_only       'select?q=*:*&fq=%7B%21frange%20u%3D2%7Drating&fl=id&sort=id%20asc&wt=json'
# incl=false -> lower bound exclusive: (2,6] -> d2,d3.
capfrange frange_incl_false       'select?q=*:*&fq=%7B%21frange%20l%3D2%20u%3D6%20incl%3Dfalse%7Drating&fl=id&sort=id%20asc&wt=json'
# incu=false -> upper bound exclusive: [2,6) -> d1,d2.
capfrange frange_incu_false       'select?q=*:*&fq=%7B%21frange%20l%3D2%20u%3D6%20incu%3Dfalse%7Drating&fl=id&sort=id%20asc&wt=json'
# Both exclusive: (2,6) -> d2.
capfrange frange_both_excl        'select?q=*:*&fq=%7B%21frange%20l%3D2%20u%3D6%20incl%3Dfalse%20incu%3Dfalse%7Drating&fl=id&sort=id%20asc&wt=json'
# No bounds at all -> every doc with an existing value. Every doc here has a
# `rating`, so this is all five: d1,d2,d3,d4,d5.
capfrange frange_no_bounds        'select?q=*:*&fq=%7B%21frange%7Drating&fl=id&sort=id%20asc&wt=json'
# Missing-excluded pinned on `price` (d4 has none): l=0 u=100 would include 0,
# yet d4 is absent -> d1(5),d2(15),d3(10),d5(8).
capfrange frange_missing_excluded 'select?q=*:*&fq=%7B%21frange%20l%3D0%20u%3D100%7Dprice&fl=id&sort=id%20asc&wt=json'
# Float bounds on `rating`: [2.5,5.5] -> d2(4),d5(5).
capfrange frange_float_bounds     'select?q=*:*&fq=%7B%21frange%20l%3D2.5%20u%3D5.5%7Drating&fl=id&sort=id%20asc&wt=json'
# Compound function over a field: product(rating,2) in [10,20] -> d3(12),d5(10).
capfrange frange_compound        'select?q=*:*&fq=%7B%21frange%20l%3D10%20u%3D20%7Dproduct%28rating%2C2%29&fl=id&sort=id%20asc&wt=json'
# Compound exists() on `price` (d4 has none): sum(price,1) in [0,15] ->
# d1(6),d3(11),d5(9). d4 is excluded EVEN THOUGH 0+1=1 would be in range --
# the exists() check sees the missing field, not the evaluated 0.
capfrange frange_compound_missing 'select?q=*:*&fq=%7B%21frange%20l%3D0%20u%3D15%7Dsum%28price%2C1%29&fl=id&sort=id%20asc&wt=json'
# Constant function references no field, so exists() is true for every doc:
# sum(1,2,3)=6 in [0,10] -> all five docs, including d5.
capfrange frange_constant         'select?q=*:*&fq=%7B%21frange%20l%3D0%20u%3D10%7Dsum%281%2C2%2C3%29&fl=id&sort=id%20asc&wt=json'
# As the main `q` (not fq): a constant-score query, every match scores 1.0.
# rating in [4,6] -> d2,d3.
capfrange frange_on_q             'select?q=%7B%21frange%20l%3D4%20u%3D6%7Drating&fl=id,score&sort=id%20asc&wt=json'

# As facet.query: the bucket key is the query string verbatim (Solr honours a
# leading `{!key=..}` for the label, but that labelled form only matters once
# #332's geodist rides through frange -- the Drupal distance-facet rewrite).
# rating in [2,6] -> 3.
capfrange frange_facet_query      'select?q=*:*&rows=0&facet=true&facet.query=%7B%21frange%20l%3D2%20u%3D6%7Drating&wt=json'
# A range no doc falls in keeps its key, at 0.
capfrange frange_facet_query_zero 'select?q=*:*&rows=0&facet=true&facet.query=%7B%21frange%20l%3D100%20u%3D200%7Drating&wt=json'

# Error shapes (400): empty body, unknown field reference, and a non-boolean
# `incl`. error.msg/metadata/trace are normalised away by the differential
# harness; only status + error.code are compared.
capfrange frange_err_empty        'select?q=*:*&fq=%7B%21frange%20l%3D2%20u%3D6%7D&fl=id&wt=json'
capfrange frange_err_unknown_field 'select?q=*:*&fq=%7B%21frange%20l%3D2%20u%3D6%7Dnosuchfield&fl=id&wt=json'
capfrange frange_err_bad_bool     'select?q=*:*&fq=%7B%21frange%20l%3D2%20u%3D6%20incl%3Dmaybe%7Drating&fl=id&wt=json'

if want_any '^frange_'; then
  release "$FRANGE_CONTAINER" "function-range core '$FRANGE_CORE'"
fi

# --- #331 spatial tracer: location field + geodist() in fl -------------------
# The thinnest retained slice of #292's spatial sizing: a `location` field
# stores a point and argless `geodist()` (driven by the `sfield`/`pt` request
# params, the client-evidenced form per finding 133) returns each doc's
# haversine distance in `fl`. Encoding decision (#292 sizing report): a
# `location` field is two synthetic f64 fast columns `<field>__lat`/`__lon`,
# reusing Wayfinder's existing fast-field machinery. `{!geofilt}`/`{!bbox}`
# and `geodist()` in `sort` are deliberately out of scope for this tracer.
#
# Own container/port/core for the same reason every other appended block uses
# one: the base `content` core has no `location` field. Port 9073: 9072 is the
# `frange` block (#333). Rows land in `manifest-errors.tsv` (not
# `manifest.tsv`) and get a dedicated `geo_app` in `tests/differential.rs`,
# like `fnq`/`pf296`. Seven docs sit on a regular grid around NYC (40,-74):
# the origin, one degree N/S/E/W, the NE corner, and a half-degree NW point.
GEO_CONTAINER=wayfinder-solr-331
GEO_SOLR=http://localhost:9073/solr
GEO_CORE=geo
if want_any '^geo_'; then
  if ! docker ps --format '{{.Names}}' | grep -qx "$GEO_CONTAINER"; then
    docker rm -f "$GEO_CONTAINER" >/dev/null 2>&1 || true
    docker run -d --name "$GEO_CONTAINER" -p 9073:8983 \
      solr:9 solr-precreate "$GEO_CORE" >/dev/null
  fi
  echo -n "waiting for geo solr"
  for _ in $(seq 60); do
    if curl -sf "$GEO_SOLR/$GEO_CORE/admin/ping?wt=json" >/dev/null 2>&1; then echo " ok"; break; fi
    echo -n "."; sleep 1
  done
  # The `_default` configset ships the `location` (LatLonPointSpatialField)
  # type and the `id` field; add `loc` as `location` *before* indexing so the
  # schemaless auto-add does not retype it to `text_general` (Wayfinder runs
  # with schemaless off; capture must match that, so the field is explicit).
  curl -s "$GEO_SOLR/$GEO_CORE/schema" -H 'Content-Type: application/json' -d '{
    "add-field": {"name":"loc","type":"location","indexed":true,"stored":true}
  }' >/dev/null
  curl -sf "$GEO_SOLR/$GEO_CORE/update?commit=true" -H 'Content-Type: application/json' -d '[
    {"id":"g1","loc":"40.0,-74.0"},
    {"id":"g2","loc":"41.0,-74.0"},
    {"id":"g3","loc":"40.0,-73.0"},
    {"id":"g4","loc":"39.0,-74.0"},
    {"id":"g5","loc":"40.0,-75.0"},
    {"id":"g6","loc":"41.0,-73.0"},
    {"id":"g7","loc":"40.5,-74.5"}
  ]' >/dev/null
fi

capg331() {  # capg331 <name> <path-after-core>
  local name=$1 suffix=$2
  want "$name" || return 0
  curl -sg "$GEO_SOLR/$GEO_CORE/$suffix" \
    -o "$OUT/$name.json" -w '%{http_code}' > "$OUT/$name.status"
  printf '%s\t%s\t%s\t%s\t%s\t%s\n' \
    "$name" "$(cat "$OUT/$name.status")" GET "$GEO_CORE/$suffix" "" "$GEO_SOLR" \
    >> "$MANIFEST_ERRORS"
  rm -f "$OUT/$name.status"
}

# `fl=<alias>:geodist()` is Solr's computed-field-in-fl form: the argless
# `geodist()` reads the point from `sfield` and the origin from `pt`, both
# request params, and emits each doc's haversine distance under `<alias>`.
# Two origins exercise the function: the grid origin (g1 -> 0.0) and the NE
# corner (g6 -> 0.0). Suffixes are percent-encoded (`(`/`)`) to match the
# encoded convention every `{!...}`/function manifest row uses.
capg331 geo_geodist_fl    'select?q=*:*&fl=id,dist:geodist%28%29&sfield=loc&pt=40,-74&sort=id%20asc&wt=json'
capg331 geo_geodist_fl_pt 'select?q=*:*&fl=id,dist:geodist%28%29&sfield=loc&pt=41,-73&sort=id%20asc&wt=json'

# `{!geofilt}`/`{!bbox}` as `fq` and `geodist()` in `sort` (issue #332), driven
# by the `sfield`/`pt`/`d` request params. `d=130` is chosen to make the
# circle-vs-square distinction observable on this 7-doc grid: the circle
# (haversine <= 130 km from 40,-74) holds g1/g2/g3/g4/g5/g7 (g2/g4 are
# ~111 km, g6 is ~140 km), while the bbox rectangle also contains g6 -- g6
# sits in the NE corner of the rectangle but outside the circle, the one doc
# `{!bbox}` returns that `{!geofilt}` does not. A second `{!geofilt}` at
# `d=70` pins the haversine boundary on g7 (~69.94 km, just inside). The
# circle/bbox blocks carry no body (their args are entirely request params),
# so the whole `fq` value is the percent-encoded `{!...}` block.
capg331 geo_geofilt        'select?q=*:*&fq=%7B%21geofilt%7D&sfield=loc&pt=40,-74&d=130&fl=id&sort=id%20asc&wt=json'
capg331 geo_bbox           'select?q=*:*&fq=%7B%21bbox%7D&sfield=loc&pt=40,-74&d=130&fl=id&sort=id%20asc&wt=json'
capg331 geo_geofilt_tight  'select?q=*:*&fq=%7B%21geofilt%7D&sfield=loc&pt=40,-74&d=70&fl=id&sort=id%20asc&wt=json'
# `sort=geodist() asc` ranks by ascending haversine distance (nearest first);
# the argless `geodist()` reads `sfield`/`pt`. `fl=id,dist:geodist()` shows
# the distance each doc sorts on, so the ordering is self-evident in the
# fixture. Ties (g3/g5 and g2/g4 are equidistant E/W of the origin) are broken
# by Solr's own (Lucene-internal) order, captured as ground truth.
capg331 geo_geodist_sort   'select?q=*:*&sfield=loc&pt=40,-74&fl=id,dist:geodist%28%29&sort=geodist%28%29%20asc&wt=json'

if want_any '^geo_'; then
  release "$GEO_CONTAINER" "geo core '$GEO_CORE'"
fi
# --- spatial heatmap facets (issue #334) ------------------------------------
# Appended block; nothing above is edited. Own container/port/core, per the
# wayfinder-solr-24 precedent: the heatmap needs an `rpts_*` field that the
# `content` core does not have, and adding one (plus a geo corpus) to `content`
# would rewrite ground truth for every doc-returning fixture. Same caveat as the
# other appended blocks: NOT runnable standalone -- `$OUT`/`$HERE` come from the
# top of the script and `caph334` appends to manifest-errors.tsv unconditionally,
# so run the whole script (or `--only '^heatmap_'`).
#
# The configset declares `location_rpt` (solr.SpatialRecursivePrefixTreeFieldType,
# geo/distErrPct/maxDistErr/distanceUnits) and the `rpts_*`/`rptm_*` dynamic
# rules (solr-ref/search-api/configset/schema.xml:240-241,435-438). solr:9's
# _default configset already ships an identical `location_rpt` type, so this
# block only adds the two dynamic rules -- no add-field-type. (If a future Solr
# drops it from _default, add the type here exactly as the configset has it.)
#
# Corpus is 10 lat,lon points: a 3-point cluster (h1-h3) to produce a count>1
# cell, four widely-spread interior points (h4-h8) to populate separate cells,
# and two boundary points -- h9 "0,0" (origin) and h10 "45,45" -- to pin Solr's
# cell-boundary convention (longitude cells are right-closed, latitude cells
# are top-closed under north-indexed rows; see finding 159). Values are written
# in the client's "lat,lon" comma form (search_api_solr's rpt format).
HEATMAP_CONTAINER=wayfinder-solr-334
HEATMAP_SOLR=http://localhost:9334/solr
HEATMAP_CORE=heatmap
# NOTE: unlike the sibling blocks above, this one ALWAYS recreates its container
# (rm -f + run) rather than reusing a running one. The corpus here IS the fixture's
# ground truth, and the reuse path re-POSTs h1..h10 without clearing what is there,
# so a leftover wayfinder-solr-334 would silently contaminate every heatmap fixture
# (stale + new docs). Recreating guarantees a clean corpus every run.
if want_any 'heatmap_'; then
  docker rm -f "$HEATMAP_CONTAINER" >/dev/null 2>&1 || true
  docker run -d --name "$HEATMAP_CONTAINER" -p 9334:8983 \
    solr:9 solr-precreate "$HEATMAP_CORE" >/dev/null
  echo -n "waiting for heatmap solr"
  for _ in $(seq 60); do
    if curl -sf "$HEATMAP_SOLR/$HEATMAP_CORE/admin/ping?wt=json" >/dev/null 2>&1; then echo " ok"; break; fi
    echo -n "."; sleep 1
  done
  curl -s "$HEATMAP_SOLR/$HEATMAP_CORE/schema" -H 'Content-Type: application/json' -d '{
    "add-dynamic-field": {"name":"rpts_*", "type":"location_rpt", "indexed":true, "stored":true, "multiValued":false},
    "add-dynamic-field": {"name":"rptm_*", "type":"location_rpt", "indexed":true, "stored":true, "multiValued":true}
  }' >/dev/null
  curl -sf "$HEATMAP_SOLR/$HEATMAP_CORE/update?commit=true" -H 'Content-Type: application/json' -d '[
    {"id":"h1","rpts_geo":"10,20"},
    {"id":"h2","rpts_geo":"10,22"},
    {"id":"h3","rpts_geo":"12,20"},
    {"id":"h4","rpts_geo":"-20,-100"},
    {"id":"h5","rpts_geo":"60,140"},
    {"id":"h6","rpts_geo":"-70,30"},
    {"id":"h7","rpts_geo":"35,-75"},
    {"id":"h8","rpts_geo":"5,130"},
    {"id":"h9","rpts_geo":"0,0"},
    {"id":"h10","rpts_geo":"45,45"}
  ]' >/dev/null
fi

# Same 6-column manifest-errors.tsv contract as capd/capf: own core, so never
# manifest.tsv (the differential harness GETs manifest.tsv rows against the
# `content` core, which has no rpts_* field).
caph334() {  # caph334 <name> <url-after-/solr/>
  local name=$1 suffix=$2
  want "$name" || return 0
  curl -sg "$HEATMAP_SOLR/$suffix" -o "$OUT/$name.json" -w '%{http_code}' > "$OUT/$name.status"
  printf '%s\t%s\t%s\t%s\t%s\t%s\n' \
    "$name" "$(cat "$OUT/$name.status")" GET "$suffix" "" "$HEATMAP_SOLR" \
    >> "$MANIFEST_ERRORS"
  rm -f "$OUT/$name.status"
}

# Grid math over the whole world at three explicit levels. The columns/rows
# here pin the geohash tree's subdivision: columns=2^ceil(5L/2), rows=2^floor
# (5L/2) -- L=1 -> 8x4, L=2 -> 32x32, L=3 -> 256x128 (finding 159). counts_ints2D
# is rows-indexed-from-NORTH, columns-from-WEST; all-zero rows are null.
caph334 heatmap_l1_world "$HEATMAP_CORE/select?q=*:*&rows=0&facet=true&facet.heatmap=rpts_geo&facet.heatmap.gridLevel=1&wt=json"
caph334 heatmap_l2_world "$HEATMAP_CORE/select?q=*:*&rows=0&facet=true&facet.heatmap=rpts_geo&facet.heatmap.gridLevel=2&wt=json"
caph334 heatmap_l3_world "$HEATMAP_CORE/select?q=*:*&rows=0&facet=true&facet.heatmap=rpts_geo&facet.heatmap.gridLevel=3&wt=json"

# Default gridLevel (no gridLevel param) is distErrPct-derived and depends on
# the geom size: whole world -> level 2, the bounded box below -> level 3. The
# client always sends gridLevel (finding 133), so this is the secondary path;
# captured to pin the default-level computation.
GEO_RECT='%5B%22-90%20-45%22%20TO%20%2290%2045%22%5D'
caph334 heatmap_default_world   "$HEATMAP_CORE/select?q=*:*&rows=0&facet=true&facet.heatmap=rpts_geo&wt=json"
caph334 heatmap_default_bounded "$HEATMAP_CORE/select?q=*:*&rows=0&facet=true&facet.heatmap=rpts_geo&facet.heatmap.geom=$GEO_RECT&wt=json"

# Bounded geom with explicit gridLevel: the grid is the world cells overlapping
# the geom bbox, snapped OUT to cell edges (minX/maxX/minY/maxY are cell-aligned,
# not the raw geom bounds). GEO_RECT is lon[-90,90] lat[-45,45].
caph334 heatmap_l1_bounded "$HEATMAP_CORE/select?q=*:*&rows=0&facet=true&facet.heatmap=rpts_geo&facet.heatmap.geom=$GEO_RECT&facet.heatmap.gridLevel=1&wt=json"
caph334 heatmap_l2_bounded "$HEATMAP_CORE/select?q=*:*&rows=0&facet=true&facet.heatmap=rpts_geo&facet.heatmap.geom=$GEO_RECT&facet.heatmap.gridLevel=2&wt=json"

# The <field>:<geom> fq constrains the doc set; the heatmap then counts only
# surviving docs. GEO_RECT keeps h1/h2/h3 (lat 10-12, lon 20-22 -- inside) and
# drops the rest, so the grid shows just that cluster.
caph334 heatmap_fq_rect "$HEATMAP_CORE/select?q=*:*&rows=0&fq=rpts_geo:$GEO_RECT&facet=true&facet.heatmap=rpts_geo&facet.heatmap.gridLevel=2&wt=json"

# Edge cases. heatmap_empty: no matching docs -> counts_ints2D is a single null
# (not an array). heatmap_format_ints2d: the default format, made explicit.
# (format=png is also a 200 in Solr but emits base64 counts_png AND stringifies
# the numeric fields -- binary, non-deterministic, never sent by the client; not
# captured, descope with a guard.)
caph334 heatmap_empty         "$HEATMAP_CORE/select?q=zzznomatch&rows=0&facet=true&facet.heatmap=rpts_geo&facet.heatmap.gridLevel=2&wt=json"
caph334 heatmap_format_ints2d "$HEATMAP_CORE/select?q=*:*&rows=0&facet=true&facet.heatmap=rpts_geo&facet.heatmap.gridLevel=2&facet.heatmap.format=ints2D&wt=json"

# maxCells is a ceiling GUARD, not a level selector: it never lowers or raises
# the level. heatmap_maxcells_guard: gridLevel=2 (32x32=1024 cells) under
# maxCells=10 -> 400 "Too many cells". heatmap_gridlevel_exceeds_maxcells:
# gridLevel=4 (1024x1024) under the default maxCells=100000 -> same 400.
caph334 heatmap_maxcells_guard             "$HEATMAP_CORE/select?q=*:*&rows=0&facet=true&facet.heatmap=rpts_geo&facet.heatmap.gridLevel=2&facet.heatmap.maxCells=10&wt=json"
caph334 heatmap_gridlevel_exceeds_maxcells "$HEATMAP_CORE/select?q=*:*&rows=0&facet=true&facet.heatmap=rpts_geo&facet.heatmap.gridLevel=4&wt=json"

# Error shapes: an undefined field, a non-spatial field, and facet.heatmap
# without facet=true (Solr silently omits facet_counts entirely).
caph334 heatmap_unknown_field    "$HEATMAP_CORE/select?q=*:*&rows=0&facet=true&facet.heatmap=nosuchfield&facet.heatmap.gridLevel=2&wt=json"
caph334 heatmap_nonspatial_field "$HEATMAP_CORE/select?q=*:*&rows=0&facet=true&facet.heatmap=id&facet.heatmap.gridLevel=2&wt=json"
caph334 heatmap_no_facet_true    "$HEATMAP_CORE/select?q=*:*&rows=0&facet.heatmap=rpts_geo&facet.heatmap.gridLevel=2&wt=json"

if want_any 'heatmap_'; then
  release "$HEATMAP_CONTAINER" "heatmap core '$HEATMAP_CORE'"
fi

# --- grouping + facet/stats/highlighting, group.truncate, group.facet (#338) --
# #290 shipped `group=true` with no other component alongside it, and accepted
# `group.truncate`/`group.facet` as documented no-ops because nothing was
# captured. `setGrouping()` sends both unconditionally (finding 130) and a
# faceted Search API view that switches grouping on sends `facet=true` in the
# same request, so the grouped-plus-components shape is real client traffic.
# This block captures it.
#
# Own container/port, but core `grouping` and the exact `g1..g6` corpus of the
# issue-#290 block above (which released its container by here), so the rows
# route to `tests/differential.rs`'s existing `grouping_app` unchanged.
# Port 9074: 9073 is the `geo` block (#331).
#
# Every row carries `sort=id asc`, which makes both the group order and each
# group's top document deterministic -- `group.truncate` facets over the top
# document of each group, so an undefined "most relevant" doc under the default
# `score desc` on an all-1.0 `q=*:*` would make the fixture order-dependent.
# With `sort=id asc` the truncated set is exactly {g1 (article), g2 (page),
# g6 (null group)}, and `fl=id` keeps `score`/`maxScore` out of the envelope.
G338_CONTAINER=wayfinder-solr-338
G338_SOLR=http://localhost:9074/solr
G338_CORE=grouping
if want_any '^g338_'; then
  if ! docker ps --format '{{.Names}}' | grep -qx "$G338_CONTAINER"; then
    docker rm -f "$G338_CONTAINER" >/dev/null 2>&1 || true
    docker run -d --name "$G338_CONTAINER" -p 9074:8983 \
      solr:9 solr-precreate "$G338_CORE" >/dev/null
  fi
  echo -n "waiting for g338 solr"
  for _ in $(seq 60); do
    if curl -sf "$G338_SOLR/$G338_CORE/admin/ping?wt=json" >/dev/null 2>&1; then echo " ok"; break; fi
    echo -n "."; sleep 1
  done
  curl -s "$G338_SOLR/$G338_CORE/schema" -H 'Content-Type: application/json' -d '{
    "add-field": [
      {"name":"body",       "type":"text_en", "indexed":true, "stored":true},
      {"name":"type",       "type":"string",  "indexed":true, "stored":true, "docValues":true},
      {"name":"category",   "type":"string",  "indexed":true, "stored":true, "docValues":true, "multiValued":true},
      {"name":"popularity", "type":"pint",    "indexed":true, "stored":true, "docValues":true}
    ]
  }' >/dev/null
  curl -sf "$G338_SOLR/$G338_CORE/update?commit=true" -H 'Content-Type: application/json' -d '[
    {"id":"g1","type":"article","category":["news"],"body":"lazy dog brown","popularity":10},
    {"id":"g2","type":"page","category":["news"],"body":"lazy garden afternoon","popularity":20},
    {"id":"g3","type":"article","category":["blog"],"body":"quick thinking saves","popularity":30},
    {"id":"g4","type":"article","category":["blog"],"body":"dogs cats together","popularity":5},
    {"id":"g5","type":"page","body":"nothing here","popularity":40},
    {"id":"g6","body":"orphan ungrouped","popularity":15}
  ]' >/dev/null
fi

capg338() {  # capg338 <name> <path-after-core>
  local name=$1 suffix=$2
  want "$name" || return 0
  curl -sg "$G338_SOLR/$G338_CORE/$suffix" \
    -o "$OUT/$name.json" -w '%{http_code}' > "$OUT/$name.status"
  printf '%s\t%s\t%s\t%s\t%s\t%s\n' \
    "$name" "$(cat "$OUT/$name.status")" GET "$G338_CORE/$suffix" "" "$G338_SOLR" \
    >> "$MANIFEST_ERRORS"
  rm -f "$OUT/$name.status"
}

G338_GRP='group=true&group.field=type&group.ngroups=true'
G338_FF='facet=true&facet.field=type&facet.field=category'
G338_TAIL='fl=id&sort=id%20asc&wt=json'

# Baselines: each component alongside the `grouped` envelope, with no
# `group.truncate`/`group.facet`. Facet counts here are plain document counts
# over the whole match set (type article=3/page=2, category news=2/blog=2),
# which is what makes the truncate/group.facet rows below readable as diffs.
capg338 g338_facet      "select?q=*:*&$G338_GRP&$G338_FF&$G338_TAIL"
capg338 g338_stats      "select?q=*:*&$G338_GRP&stats=true&stats.field=popularity&$G338_TAIL"
capg338 g338_all        "select?q=*:*&$G338_GRP&$G338_FF&stats=true&stats.field=popularity&$G338_TAIL"
# Highlighting: `q=lazy` (text_en, so `body` stems) matches g1 (article) and
# g2 (page). `highlighting` is keyed by document id at the top level, the same
# as the ungrouped envelope -- this pins whether it covers only the documents
# the doclists actually returned.
capg338 g338_hl         "select?q=lazy&df=body&$G338_GRP&group.limit=2&hl=true&hl.fl=body&$G338_TAIL"
capg338 g338_hl_facet   "select?q=lazy&df=body&$G338_GRP&group.limit=2&hl=true&hl.fl=body&$G338_FF&$G338_TAIL"
# No match: pins the empty-facet/empty-stats shape next to an empty `grouped`.
capg338 g338_zero       "select?q=zzznomatch&df=body&$G338_GRP&$G338_FF&stats=true&stats.field=popularity&$G338_TAIL"

# `group.truncate=true`: facet counts over the collapsed group set (the top
# document of each group under `sort=id asc`: g1, g2, g6) rather than the
# matching document set. Expected to move `type` from article=3/page=2 to
# article=1/page=1 and `category` from news=2/blog=2 to news=2/blog=0.
capg338 g338_truncate       "select?q=*:*&$G338_GRP&group.truncate=true&$G338_FF&$G338_TAIL"
# Whether `stats` follows `group.truncate` too, or stays over the full match
# set, is exactly what this row decides.
capg338 g338_truncate_stats "select?q=*:*&$G338_GRP&group.truncate=true&stats=true&stats.field=popularity&$G338_TAIL"
capg338 g338_truncate_false "select?q=*:*&$G338_GRP&group.truncate=false&$G338_FF&$G338_TAIL"

# `group.facet=true`: field-facet counts become the number of GROUPS holding at
# least one matching document with that value, not the document count.
# `category` separates the two cleanly: news is on g1 (article) and g2 (page)
# -> 2 groups, blog is on g3 and g4, both article -> 1 group, where the
# document counts are 2 and 2. Faceting on the group field itself is the other
# half: every `type` value is its own group, so each count is 1.
capg338 g338_groupfacet          "select?q=*:*&$G338_GRP&group.facet=true&$G338_FF&$G338_TAIL"
capg338 g338_groupfacet_truncate "select?q=*:*&$G338_GRP&group.facet=true&group.truncate=true&$G338_FF&$G338_TAIL"
# `group.facet` is documented as applying to FIELD facets only. These two rows
# pin whether `facet.query` and `facet.range` counts stay document counts under
# it: the same pair of requests with and without the flag.
capg338 g338_facet_qr       "select?q=*:*&$G338_GRP&facet=true&facet.query=category:news&facet.range=popularity&f.popularity.facet.range.start=0&f.popularity.facet.range.end=50&f.popularity.facet.range.gap=25&$G338_TAIL"
capg338 g338_groupfacet_qr  "select?q=*:*&$G338_GRP&group.facet=true&facet=true&facet.query=category:news&facet.range=popularity&f.popularity.facet.range.start=0&f.popularity.facet.range.end=50&f.popularity.facet.range.gap=25&$G338_TAIL"

# `category:news` is on g1 (article) and g2 (page) -- 2 documents AND 2 groups,
# so the `g338_*_qr` pair above cannot tell a grouped `facet.query` count from a
# document count. `category:blog` can: g3 and g4 are 2 documents but both are
# `article`, so a grouped count would be 1. Same disambiguation for
# `group.truncate` (the truncated set {g1,g2,g6} holds no blog document at all).
G338_QR='facet=true&facet.query=category:blog&facet.range=popularity&f.popularity.facet.range.start=0&f.popularity.facet.range.end=50&f.popularity.facet.range.gap=25'
capg338 g338_facet_blog      "select?q=*:*&$G338_GRP&$G338_QR&$G338_TAIL"
capg338 g338_groupfacet_blog "select?q=*:*&$G338_GRP&group.facet=true&$G338_QR&$G338_TAIL"
capg338 g338_truncate_qr     "select?q=*:*&$G338_GRP&group.truncate=true&$G338_QR&$G338_TAIL"
# Two `group.field` values with `group.facet=true`: Solr documents grouped
# facets as computed from the FIRST specified group field. `type` gives 3
# groups, `popularity` gives 6 (every value distinct), so which one drives the
# counts is observable.
# Which document is a group's "most relevant" one for `group.truncate` -- the
# one the main `sort` picks, or the one `group.sort` picks? `group.sort=id desc`
# makes article's top document g4 (category blog) instead of g1 (news), which
# moves the `category` facet, so the fixture names the answer.
capg338 g338_truncate_groupsort "select?q=*:*&$G338_GRP&group.truncate=true&group.sort=id+desc&$G338_FF&$G338_TAIL"
# Is the truncated/grouped set the WHOLE group set or only the page of groups
# `rows` returns? `rows=1` returns one group; if truncation were page-scoped the
# `type` facet would be `article=1` alone. Same question for `group.facet`.
capg338 g338_truncate_rows   "select?q=*:*&$G338_GRP&group.truncate=true&rows=1&$G338_FF&$G338_TAIL"
capg338 g338_groupfacet_rows "select?q=*:*&$G338_GRP&group.facet=true&rows=1&$G338_FF&$G338_TAIL"
# Which `group.field` does `group.truncate` collapse on when there are two?
# `type` collapses 6 documents to 3, `popularity` leaves all 6 (every value is
# distinct), so the facet counts name the answer.
capg338 g338_truncate_multi  "select?q=*:*&group=true&group.field=type&group.field=popularity&group.ngroups=true&group.truncate=true&$G338_FF&$G338_TAIL"
# `group.facet` is a faceting flag; this pins that it leaves `stats` alone
# (unlike `group.truncate`, which `g338_truncate_stats` shows stats does follow).
capg338 g338_groupfacet_stats "select?q=*:*&$G338_GRP&group.facet=true&stats=true&stats.field=popularity&$G338_TAIL"
capg338 g338_groupfacet_multi "select?q=*:*&group=true&group.field=type&group.field=popularity&group.ngroups=true&group.facet=true&$G338_FF&$G338_TAIL"

# `{!ex=...}` multi-select faceting (#295) crossed with `group.facet`/
# `group.truncate` (#338). An excluded facet counts against a REDUCED filter
# set -- a superset of the documents the grouping pass bucketed -- so the two
# features interact and nothing captured pins the result. `fq={!tag=t}` is
# `category:news` (g1/article, g2/page), and the excluded facets count over the
# full `*:*` set instead: `category` news is g1+g2 = 2 groups, blog is g3+g4,
# both `article` = 1 group, and `facet.query=category:blog` is likewise 2
# documents but 1 group. The unexcluded `type` facet still counts against the
# filtered set. Percent-encoded `{`/`}`/`!` like every other local-param row.
G338_EX_FQ='fq=%7B%21tag%3Dt%7Dcategory%3Anews'
G338_EX_F='facet=true&facet.field=%7B%21ex%3Dt%7Dcategory&facet.field=type&facet.query=%7B%21ex%3Dt%7Dcategory%3Ablog'
capg338 g338_ex_facet      "select?q=*:*&$G338_GRP&$G338_EX_FQ&$G338_EX_F&$G338_TAIL"
capg338 g338_ex_groupfacet "select?q=*:*&$G338_GRP&group.facet=true&$G338_EX_FQ&$G338_EX_F&$G338_TAIL"
capg338 g338_ex_truncate   "select?q=*:*&$G338_GRP&group.truncate=true&$G338_EX_FQ&$G338_EX_F&$G338_TAIL"
capg338 g338_ex_both       "select?q=*:*&$G338_GRP&group.facet=true&group.truncate=true&$G338_EX_FQ&$G338_EX_F&$G338_TAIL"

if want_any '^g338_'; then
  release "$G338_CONTAINER" "g338 core '$G338_CORE'"
fi

# --- group.facet over a null group that HAS facet values (#338) --------------
# The `grouping` corpus above cannot exercise one half of `group.facet`: its
# only null-group document (g6) carries no `type` AND no `category`, so no
# captured field facet ever has a term sitting on a document that is missing
# the group field. A `group.field` terms sub-aggregation structurally cannot see
# that group (it has no group term), so an implementation that forgets to add
# the missing-value group back stays green on every `g338_*` row -- confirmed by
# mutation-testing the correction away. This corpus closes that gap.
#
# `type` groups h1/h2 as article, h3 as page, and leaves h4/h5 in the null
# group. `category` news is on h1 (article), h3 (page), h4 and h5 (both null) --
# 4 documents but 3 groups, which is the number that only a correct null-group
# correction produces. blog is on h2 alone: 1 document, 1 group.
# Own container/port 9075 and own core `g338null`, so nothing above is disturbed.
G338N_CONTAINER=wayfinder-solr-338-null
G338N_SOLR=http://localhost:9075/solr
G338N_CORE=g338null
if want_any '^g338n_'; then
  if ! docker ps --format '{{.Names}}' | grep -qx "$G338N_CONTAINER"; then
    docker rm -f "$G338N_CONTAINER" >/dev/null 2>&1 || true
    docker run -d --name "$G338N_CONTAINER" -p 9075:8983 \
      solr:9 solr-precreate "$G338N_CORE" >/dev/null
  fi
  echo -n "waiting for g338null solr"
  for _ in $(seq 60); do
    if curl -sf "$G338N_SOLR/$G338N_CORE/admin/ping?wt=json" >/dev/null 2>&1; then echo " ok"; break; fi
    echo -n "."; sleep 1
  done
  curl -s "$G338N_SOLR/$G338N_CORE/schema" -H 'Content-Type: application/json' -d '{
    "add-field": [
      {"name":"body",       "type":"text_en", "indexed":true, "stored":true},
      {"name":"type",       "type":"string",  "indexed":true, "stored":true, "docValues":true},
      {"name":"category",   "type":"string",  "indexed":true, "stored":true, "docValues":true, "multiValued":true},
      {"name":"popularity", "type":"pint",    "indexed":true, "stored":true, "docValues":true}
    ]
  }' >/dev/null
  curl -sf "$G338N_SOLR/$G338N_CORE/update?commit=true" -H 'Content-Type: application/json' -d '[
    {"id":"h1","type":"article","category":["news"],"body":"first","popularity":10},
    {"id":"h2","type":"article","category":["blog"],"body":"second","popularity":20},
    {"id":"h3","type":"page","category":["news"],"body":"third","popularity":30},
    {"id":"h4","category":["news"],"body":"fourth","popularity":40},
    {"id":"h5","category":["news"],"body":"fifth","popularity":50}
  ]' >/dev/null
fi

capg338n() {  # capg338n <name> <path-after-core>
  local name=$1 suffix=$2
  want "$name" || return 0
  curl -sg "$G338N_SOLR/$G338N_CORE/$suffix" \
    -o "$OUT/$name.json" -w '%{http_code}' > "$OUT/$name.status"
  printf '%s\t%s\t%s\t%s\t%s\t%s\n' \
    "$name" "$(cat "$OUT/$name.status")" GET "$G338N_CORE/$suffix" "" "$G338N_SOLR" \
    >> "$MANIFEST_ERRORS"
  rm -f "$OUT/$name.status"
}

G338N_GRP='group=true&group.field=type&group.ngroups=true'
G338N_FF='facet=true&facet.field=type&facet.field=category'
G338N_TAIL='fl=id&sort=id%20asc&wt=json'

# Baseline document counts, then the same request under `group.facet=true`.
# `category` news must go 4 -> 3, which is only reachable by counting the null
# group; blog stays 1 -> 1.
capg338n g338n_facet      "select?q=*:*&$G338N_GRP&$G338N_FF&$G338N_TAIL"
capg338n g338n_groupfacet "select?q=*:*&$G338N_GRP&group.facet=true&$G338N_FF&$G338N_TAIL"
# `facet.missing=true` alongside `group.facet`: the `type` facet's missing
# bucket covers h4/h5, which are one group (the null group), not two documents.
capg338n g338n_groupfacet_missing "select?q=*:*&$G338N_GRP&group.facet=true&$G338N_FF&facet.missing=true&$G338N_TAIL"
capg338n g338n_facet_missing      "select?q=*:*&$G338N_GRP&$G338N_FF&facet.missing=true&$G338N_TAIL"
# And the truncate half on this corpus: collapsed set is {h1, h3, h4}
# (`sort=id asc`), so `category` news = 3, blog = 0.
capg338n g338n_truncate "select?q=*:*&$G338N_GRP&group.truncate=true&$G338N_FF&$G338N_TAIL"

if want_any '^g338n_'; then
  release "$G338N_CONTAINER" "g338null core '$G338N_CORE'"
fi

# --- {!payload_score} over a boost_term_payload field (#340)
# The fourth and last concrete function the 4.4.0 snapshot names (finding 129);
# findings 143-146 split it out of #289 because it is a *query parser* over a
# payload-bearing field type, not an arithmetic function.
#
# The field type is copied verbatim from the module's own configset
# (`solr-conf-templates/9.x/schema.xml:387-406`, fetched from `git.drupalcode.org`
# `4.4.x`): whitespace tokenizer, LengthFilter min=2/max=100, lowercase,
# RemoveDuplicates, then DelimitedPayloadTokenFilter with the float encoder. The
# `boost_term` field is `multiValued`, `stored=false` (`schema.xml:157`), and the
# indexing side writes one value per boosted term as `sprintf('%s|%.1F')`
# (`SearchApiSolrBackend.php:1502-1503`), which is why every corpus value below
# is a single `<term>|<boost>` token.
#
# Own container/port/core: no existing corpus has a payload-bearing field.
# Rows land in `manifest-errors.tsv` and get a dedicated `pls_app` in
# `tests/differential.rs`, like `fnq_*`.
#
# d3 carries `dog` twice with different payloads (1.5 and 4.5) -- that is the
# only way min/max/average/sum are distinguishable from each other. d4 has no
# `boost_term` at all, so it never matches a payload_score clause.
PLS_CONTAINER=wayfinder-solr-340
PLS_SOLR=http://localhost:9076/solr
PLS_CORE=pls
if want_any '^pls_'; then
  if ! docker ps --format '{{.Names}}' | grep -qx "$PLS_CONTAINER"; then
    docker rm -f "$PLS_CONTAINER" >/dev/null 2>&1 || true
    docker run -d --name "$PLS_CONTAINER" -p 9076:8983 \
      solr:9 solr-precreate "$PLS_CORE" >/dev/null
  fi
  echo -n "waiting for payload_score solr"
  for _ in $(seq 60); do
    if curl -sf "$PLS_SOLR/$PLS_CORE/admin/ping?wt=json" >/dev/null 2>&1; then echo " ok"; break; fi
    echo -n "."; sleep 1
  done
  curl -s "$PLS_SOLR/$PLS_CORE/schema" -H 'Content-Type: application/json' -d '{
    "add-field-type": {
      "name":"boost_term_payload","class":"solr.TextField","stored":false,"indexed":true,
      "analyzer":{
        "tokenizer":{"class":"solr.WhitespaceTokenizerFactory"},
        "filters":[
          {"class":"solr.LengthFilterFactory","min":"2","max":"100"},
          {"class":"solr.LowerCaseFilterFactory"},
          {"class":"solr.RemoveDuplicatesTokenFilterFactory"},
          {"class":"solr.DelimitedPayloadTokenFilterFactory","encoder":"float"}
        ]
      }
    }
  }' >/dev/null
  curl -s "$PLS_SOLR/$PLS_CORE/schema" -H 'Content-Type: application/json' -d '{
    "add-field": [
      {"name":"body","type":"text_general","indexed":true,"stored":true},
      {"name":"boost_document","type":"pfloat","indexed":true,"stored":true,"docValues":true},
      {"name":"boost_term","type":"boost_term_payload","indexed":true,"stored":false,"multiValued":true}
    ]
  }' >/dev/null
  curl -sf "$PLS_SOLR/$PLS_CORE/update?commit=true" -H 'Content-Type: application/json' -d '[
    {"id":"d1","body":"quick brown fox","boost_document":1.0,"boost_term":["fox|2.0","brown|1.5"]},
    {"id":"d2","body":"lazy dog","boost_document":1.0,"boost_term":["dog|3.0"]},
    {"id":"d3","body":"quick dog","boost_document":2.0,"boost_term":["dog|1.5","dog|4.5","quick|2.5"]},
    {"id":"d4","body":"quick fox","boost_document":0.5},
    {"id":"d5","body":"lazy brown","boost_document":1.0,"boost_term":["brown|4.0","lazy|0.5"]}
  ]' >/dev/null
fi

cappls() {  # cappls <name> <path-after-core>
  local name=$1 suffix=$2
  want "$name" || return 0
  curl -sg "$PLS_SOLR/$PLS_CORE/$suffix" \
    -o "$OUT/$name.json" -w '%{http_code}' > "$OUT/$name.status"
  printf '%s\t%s\t%s\t%s\t%s\t%s\n' \
    "$name" "$(cat "$OUT/$name.status")" GET "$PLS_CORE/$suffix" "" "$PLS_SOLR" \
    >> "$MANIFEST_ERRORS"
  rm -f "$OUT/$name.status"
}

# --- solr.DateRangeField intervals (issue #341) ------------------------------
# Appended block; nothing above is edited. Own container/port/core, per the
# wayfinder-solr-24 precedent: `date_range` is NOT in solr:9's `_default`
# configset (probed: no fieldType of class solr.DateRangeField ships there), so
# this block must `add-field-type` both types itself, and the `drs_*`/`drm_*`
# dynamic rules with them. Adding either to the `content` core would rewrite
# ground truth for every doc-returning fixture. Same caveat as the other
# appended blocks: NOT runnable standalone -- `$OUT`/`$HERE` come from the top
# of the script -- so run the whole script, or `--only '^dr341_'`.
#
# Types and dynamic rules are declared exactly as the captured Drupal configset
# has them (solr-ref/search-api/configset/schema.xml:199-200,340-341):
# `date_range` / `date_ranges` (multiValued), `drs_*` / `drm_*`, both
# indexed+stored with NO docValues.
#
# The corpus deliberately mixes every input form the type accepts, because Solr
# stores the value VERBATIM (d1 comes back as "2020", not as an expanded
# interval) and because a date literal denotes the whole interval of its stated
# precision -- which is the single rule the whole comparison surface follows:
#   d1 "2020"                 -> [2020-01-01T00:00:00Z .. 2020-12-31T23:59:59.999Z]
#   d2 "2020-06"              -> [2020-06-01T00:00:00Z .. 2020-06-30T23:59:59.999Z]
#   d3 explicit closed interval, Mar-Sep 2020
#   d4 "2020-06-15T12:00:00Z" -> that whole SECOND, not an instant
#   d5 open lower bound, d6 open upper bound, d7 fully open
#   d8 the multiValued case (drm_x), two DISJOINT intervals on one doc
#   d9 the same field with a SINGLE member, "2020"
# d5/d6 are what make Intersects distinguishable from a naive overlap test, and
# d7 is what pins that a fully-open interval intersects everything. d8-vs-d9 is
# what pins the multiValued rule (see the multiValued block below): they differ
# only in whether the extra 2022-05 member is present, so a predicate that
# separates them can only be reading the whole value set, not one member.
DR341_CONTAINER=wayfinder-solr-341
DR341_SOLR=http://localhost:9341/solr
DR341_CORE=daterange
# Like the heatmap block and unlike the older siblings, this ALWAYS recreates its
# container (rm -f + run) rather than reusing a running one. The corpus IS the
# ground truth of every fixture here, and the reuse path would re-POST d1..d8
# without clearing what is there, so a leftover wayfinder-solr-341 would
# silently contaminate every fixture with stale docs.
if want_any '^dr341_'; then
  docker rm -f "$DR341_CONTAINER" >/dev/null 2>&1 || true
  docker run -d --name "$DR341_CONTAINER" -p 9341:8983 \
    solr:9 solr-precreate "$DR341_CORE" >/dev/null
  echo -n "waiting for date-range solr"
  for _ in $(seq 60); do
    if curl -sf "$DR341_SOLR/$DR341_CORE/admin/ping?wt=json" >/dev/null 2>&1; then echo " ok"; break; fi
    echo -n "."; sleep 1
  done
  # Two separate schema calls: `add-field-type` for a type a dynamic rule in the
  # SAME payload references is accepted by Solr, but keeping the single-valued
  # and multiValued halves apart makes a failure in either attributable.
  curl -s "$DR341_SOLR/$DR341_CORE/schema" -H 'Content-Type: application/json' -d '{
    "add-field-type": {"name":"date_range", "class":"solr.DateRangeField"},
    "add-dynamic-field": {"name":"drs_*", "type":"date_range", "indexed":true, "stored":true, "multiValued":false}
  }' >/dev/null
  curl -s "$DR341_SOLR/$DR341_CORE/schema" -H 'Content-Type: application/json' -d '{
    "add-field-type": {"name":"date_ranges", "class":"solr.DateRangeField", "multiValued":true},
    "add-dynamic-field": {"name":"drm_*", "type":"date_ranges", "indexed":true, "stored":true, "multiValued":true}
  }' >/dev/null
  curl -sf "$DR341_SOLR/$DR341_CORE/update?commit=true" -H 'Content-Type: application/json' -d '[
    {"id":"d1","drs_x":"2020"},
    {"id":"d2","drs_x":"2020-06"},
    {"id":"d3","drs_x":"[2020-03-01T00:00:00Z TO 2020-09-30T00:00:00Z]"},
    {"id":"d4","drs_x":"2020-06-15T12:00:00Z"},
    {"id":"d5","drs_x":"[* TO 2019-12-31T23:59:59Z]"},
    {"id":"d6","drs_x":"[2021-01-01T00:00:00Z TO *]"},
    {"id":"d7","drs_x":"[* TO *]"},
    {"id":"d8","drm_x":["2020","2022-05"]},
    {"id":"d9","drm_x":["2020"]}
  ]' >/dev/null
fi

# Same 6-column manifest-errors.tsv contract as caph334/capg338n: own core, so
# never manifest.tsv (the differential harness GETs manifest.tsv rows against the
# `content` core, which has no drs_* field).
capdr341() {  # capdr341 <name> <url-after-/solr/>
  local name=$1 suffix=$2
  want "$name" || return 0
  curl -sg "$DR341_SOLR/$suffix" -o "$OUT/$name.json" -w '%{http_code}' > "$OUT/$name.status"
  printf '%s\t%s\t%s\t%s\t%s\t%s\n' \
    "$name" "$(cat "$OUT/$name.status")" GET "$suffix" "" "$DR341_SOLR" \
    >> "$MANIFEST_ERRORS"
  rm -f "$OUT/$name.status"
}

# Percent-encoded for the same reason the fnq block is: `{`/`}`/`!`/space/`"`
# and the inner `=` would otherwise break the in-process axum URI the
# differential harness replays. `%22` is the double quote the module's
# `escapePhrase()` puts around every `v` value.
PLS_TAIL='fl=id,score&sort=score%20desc,id%20asc&wt=json'

# The four payload functions. Only d3 distinguishes them (payloads 1.5, 4.5):
# max=4.5, min=1.5, average=3.0, sum=6.0. d2 has a single 3.0 payload, so it
# scores 3.0 under all four -- which is what makes it the control.
cappls pls_max     "select?q=%7B%21payload_score%20f%3Dboost_term%20v%3D%22dog%22%20func%3Dmax%7D&$PLS_TAIL"
cappls pls_min     "select?q=%7B%21payload_score%20f%3Dboost_term%20v%3D%22dog%22%20func%3Dmin%7D&$PLS_TAIL"
cappls pls_average "select?q=%7B%21payload_score%20f%3Dboost_term%20v%3D%22dog%22%20func%3Daverage%7D&$PLS_TAIL"
cappls pls_sum     "select?q=%7B%21payload_score%20f%3Dboost_term%20v%3D%22dog%22%20func%3Dsum%7D&$PLS_TAIL"

# `includeSpanScore` defaults to *false* on solr:9 -- the bare form and the
# explicit `false` form both score the raw payload value, with no BM25 factor.
# That is what keeps these fixtures exactly comparable rather than sitting under
# the ratified BM25-magnitude divergence. `includeSpanScore=true` multiplies the
# span score back in and is the one row expected to diverge.
cappls pls_span_false "select?q=%7B%21payload_score%20f%3Dboost_term%20v%3D%22dog%22%20func%3Dmax%20includeSpanScore%3Dfalse%7D&$PLS_TAIL"
cappls pls_span_true  "select?q=%7B%21payload_score%20f%3Dboost_term%20v%3D%22dog%22%20func%3Dmax%20includeSpanScore%3Dtrue%7D&$PLS_TAIL"

# `v` is analyzed by the field type, so an uppercase value still matches
# (LowerCaseFilter); the quotes `escapePhrase()` adds are optional as far as the
# parser is concerned. A doc with no `boost_term` (d4) and a term nothing
# carries both yield an empty result rather than a zero score.
cappls pls_v_unquoted "select?q=%7B%21payload_score%20f%3Dboost_term%20v%3Ddog%20func%3Dmax%7D&$PLS_TAIL"
cappls pls_v_upper    "select?q=%7B%21payload_score%20f%3Dboost_term%20v%3D%22DOG%22%20func%3Dmax%7D&$PLS_TAIL"
cappls pls_unmatched  "select?q=%7B%21payload_score%20f%3Dboost_term%20v%3D%22nosuch%22%20func%3Dmax%7D&$PLS_TAIL"

# Two payload_score blocks with nothing in front of them. A position-0 local
# params block sets the parser for the *whole* `q`, so the first block consumes
# the rest of the string and -- because `v` is given explicitly -- discards it:
# d3 scores dog's 4.5, not dog(4.5)+quick(2.5)=7.0. Only when something else
# holds position 0 (see `pls_client_shape`) do the blocks become clauses the
# lucene parser sums. This row is the one that pins that asymmetry.
cappls pls_two_terms "select?q=%7B%21payload_score%20f%3Dboost_term%20v%3D%22dog%22%20func%3Dmax%7D%20%7B%21payload_score%20f%3Dboost_term%20v%3D%22quick%22%20func%3Dmax%7D&$PLS_TAIL"

# The exact string `SearchApiSolrBackend::preQuery` assembles when a term-boost
# processor is configured (`SearchApiSolrBackend.php:1947-1981` joined with
# spaces): a position-0 `{!boost b=boost_document}` whose child is the two
# inline `{!payload_score}` clauses plus the `*:*` fallback. Every clause is a
# SHOULD, so the child score is dog+quick+1.0 and the wrapper multiplies by
# `boost_document`: d3 = (4.5+2.5+1.0)*2.0 = 16.0, d2 = (3.0+1.0)*1.0 = 4.0,
# d1/d5 = 1.0, d4 = 0.5. This is the row the whole increment exists for -- note
# the payload_score blocks are *inline*, not at position 0.
cappls pls_client_shape "select?q=%7B%21boost%20b%3Dboost_document%7D%20%7B%21payload_score%20f%3Dboost_term%20v%3D%22dog%22%20func%3Dmax%7D%20%7B%21payload_score%20f%3Dboost_term%20v%3D%22quick%22%20func%3Dmax%7D%20*%3A*&$PLS_TAIL"

# A multi-term `v` becomes an ordered SpanNearQuery over the payload field.
# `positionIncrementGap` is unset on the field type, so consecutive multiValued
# values sit at consecutive positions and d3's `dog`(pos 1)/`quick`(pos 2) form
# a span; the function runs over the payloads inside it (max 4.5). The module
# never emits this -- every `boost_term` value is a single token -- so it is a
# named descope with an expiring guard, not a supported form.
cappls pls_multiterm_v "select?q=%7B%21payload_score%20f%3Dboost_term%20v%3D%22dog%20quick%22%20func%3Dmax%7D&$PLS_TAIL"

# Error shapes. `f` missing is a plain 400 `'f' not specified`; a `v` that
# analyzes to nothing -- absent entirely, or shorter than LengthFilter's min=2
# -- is 400 `SpanQuery is null`, the exception the module's own comments warn
# about (`Utility.php:1004-1007`). `func` is case-sensitive and required.
cappls pls_err_no_f          "select?q=%7B%21payload_score%20v%3D%22dog%22%20func%3Dmax%7D&$PLS_TAIL"
cappls pls_err_no_v          "select?q=%7B%21payload_score%20f%3Dboost_term%20func%3Dmax%7D&$PLS_TAIL"
cappls pls_err_short_v       "select?q=%7B%21payload_score%20f%3Dboost_term%20v%3D%22a%22%20func%3Dmax%7D&$PLS_TAIL"
cappls pls_err_no_func       "select?q=%7B%21payload_score%20f%3Dboost_term%20v%3D%22dog%22%7D&$PLS_TAIL"
cappls pls_err_unknown_func  "select?q=%7B%21payload_score%20f%3Dboost_term%20v%3D%22dog%22%20func%3Dbogus%7D&$PLS_TAIL"
cappls pls_err_func_case     "select?q=%7B%21payload_score%20f%3Dboost_term%20v%3D%22dog%22%20func%3DMAX%7D&$PLS_TAIL"
cappls pls_err_undef_field   "select?q=%7B%21payload_score%20f%3Dnosuchfield%20v%3D%22dog%22%20func%3Dmax%7D&$PLS_TAIL"
# `f` naming a real field that carries no payloads is an uncaught
# NullPointerException in Lucene's PayloadScoreQuery constructor -- HTTP 500,
# not 400. Captured as evidence; Wayfinder answers 400 rather than reproducing
# an upstream crash, which is a ratified divergence, not a bug.
cappls pls_err_nonpayload    "select?q=%7B%21payload_score%20f%3Dbody%20v%3D%22dog%22%20func%3Dmax%7D&$PLS_TAIL"

if want_any '^pls_'; then
  release "$PLS_CONTAINER" "payload_score core '$PLS_CORE'"
fi

# --- payload-free occurrences in a boost_term_payload field (#340, finding 172)
# The `pls` corpus writes every value as `<term>|<boost>`, because that is all
# the module ever emits (`sprintf('%s|%.1F')`). It therefore says nothing about
# what a *payload-free* occurrence scores -- and the natural guesses are both
# wrong. Solr's PayloadDecoder decodes a null payload to `1f` rather than
# skipping the position, so a bare token contributes the factor 1.0 and takes
# part in the aggregate like any other.
#
# Own container/port/core rather than three more docs in `pls`, so none of the
# committed `pls_*` fixtures move.
#
# z1 has the bare token alone; z2 is the payloaded control; z3 carries both
# forms of the same term, which is the row that distinguishes "contributes 1.0"
# from "is skipped" -- under `min`, skipping would give 2.0 and contributing
# gives 1.0.
PLSZ_CONTAINER=wayfinder-solr-340z
PLSZ_SOLR=http://localhost:9077/solr
PLSZ_CORE=plsz
if want_any '^plsz_'; then
  if ! docker ps --format '{{.Names}}' | grep -qx "$PLSZ_CONTAINER"; then
    docker rm -f "$PLSZ_CONTAINER" >/dev/null 2>&1 || true
    docker run -d --name "$PLSZ_CONTAINER" -p 9077:8983 \
      solr:9 solr-precreate "$PLSZ_CORE" >/dev/null
  fi
  echo -n "waiting for payload-free solr"
  for _ in $(seq 60); do
    if curl -sf "$PLSZ_SOLR/$PLSZ_CORE/admin/ping?wt=json" >/dev/null 2>&1; then echo " ok"; break; fi
    echo -n "."; sleep 1
  done
  # Field type identical to the `pls` block's -- same verbatim copy of the
  # module's configset, so the only variable under test is the corpus.
  curl -s "$PLSZ_SOLR/$PLSZ_CORE/schema" -H 'Content-Type: application/json' -d '{
    "add-field-type": {
      "name":"boost_term_payload","class":"solr.TextField","stored":false,"indexed":true,
      "analyzer":{
        "tokenizer":{"class":"solr.WhitespaceTokenizerFactory"},
        "filters":[
          {"class":"solr.LengthFilterFactory","min":"2","max":"100"},
          {"class":"solr.LowerCaseFilterFactory"},
          {"class":"solr.RemoveDuplicatesTokenFilterFactory"},
          {"class":"solr.DelimitedPayloadTokenFilterFactory","encoder":"float"}
        ]
      }
    }
  }' >/dev/null
  curl -s "$PLSZ_SOLR/$PLSZ_CORE/schema" -H 'Content-Type: application/json' -d '{
    "add-field": [
      {"name":"boost_term","type":"boost_term_payload","indexed":true,"stored":false,"multiValued":true}
    ]
  }' >/dev/null
  curl -sf "$PLSZ_SOLR/$PLSZ_CORE/update?commit=true" -H 'Content-Type: application/json' -d '[
    {"id":"z1","boost_term":["dog"]},
    {"id":"z2","boost_term":["dog|3.0"]},
    {"id":"z3","boost_term":["cat","cat|2.0"]},
    {"id":"z4","boost_term":["bird|2.0","bird|2.0"]}
  ]' >/dev/null
fi

capplsz() {  # capplsz <name> <path-after-core>
  local name=$1 suffix=$2
  want "$name" || return 0
  curl -sg "$PLSZ_SOLR/$PLSZ_CORE/$suffix" \
    -o "$OUT/$name.json" -w '%{http_code}' > "$OUT/$name.status"
  printf '%s\t%s\t%s\t%s\t%s\t%s\n' \
    "$name" "$(cat "$OUT/$name.status")" GET "$PLSZ_CORE/$suffix" "" "$PLSZ_SOLR" \
    >> "$MANIFEST_ERRORS"
  rm -f "$OUT/$name.status"
}

PLSZ_TAIL='fl=id,score&sort=score%20desc,id%20asc&wt=json'

# `dog`: z1 (bare) against z2 (payload 3.0). z1 scores 1.0 under all four
# functions -- not 0.0, and not absent from the results.
capplsz plsz_bare_max     "select?q=%7B%21payload_score%20f%3Dboost_term%20v%3D%22dog%22%20func%3Dmax%7D&$PLSZ_TAIL"
capplsz plsz_bare_min     "select?q=%7B%21payload_score%20f%3Dboost_term%20v%3D%22dog%22%20func%3Dmin%7D&$PLSZ_TAIL"
capplsz plsz_bare_average "select?q=%7B%21payload_score%20f%3Dboost_term%20v%3D%22dog%22%20func%3Daverage%7D&$PLSZ_TAIL"
capplsz plsz_bare_sum     "select?q=%7B%21payload_score%20f%3Dboost_term%20v%3D%22dog%22%20func%3Dsum%7D&$PLSZ_TAIL"

# `cat`: z3 alone, carrying both `cat` and `cat|2.0`. The aggregate runs over
# [1.0, 2.0] -- max 2.0, min 1.0, average 1.5, sum 3.0. These four values are
# the whole evidence for the "decodes to 1f, does not skip" rule.
capplsz plsz_mixed_max     "select?q=%7B%21payload_score%20f%3Dboost_term%20v%3D%22cat%22%20func%3Dmax%7D&$PLSZ_TAIL"
capplsz plsz_mixed_min     "select?q=%7B%21payload_score%20f%3Dboost_term%20v%3D%22cat%22%20func%3Dmin%7D&$PLSZ_TAIL"
capplsz plsz_mixed_average "select?q=%7B%21payload_score%20f%3Dboost_term%20v%3D%22cat%22%20func%3Daverage%7D&$PLSZ_TAIL"
capplsz plsz_mixed_sum     "select?q=%7B%21payload_score%20f%3Dboost_term%20v%3D%22cat%22%20func%3Dsum%7D&$PLSZ_TAIL"

# Solr's general local-params contract says a block with no `v` takes its query
# text from the *bound* run after `}`. `pls_err_no_v` shows the 400 when there
# is no bound text either, but says nothing about the case where there is one --
# so these two rows settle whether `{!payload_score f=... func=max}dog` is a
# working query and whether an explicit `v` beats a bound run that disagrees
# with it. The module always emits an explicit `v`
# (`Utility::flattenKeysToPayloadScore`), so this is contract coverage rather
# than client-path coverage.
capplsz plsz_vbound_max    "select?q=%7B%21payload_score%20f%3Dboost_term%20func%3Dmax%7Ddog&$PLSZ_TAIL"
capplsz plsz_vbound_v_wins "select?q=%7B%21payload_score%20f%3Dboost_term%20v%3D%22cat%22%20func%3Dmax%7Ddog&$PLSZ_TAIL"

# Two *identical* `<term>|<boost>` values. `RemoveDuplicatesTokenFilter` drops
# duplicates only within one position, and consecutive multiValued values sit at
# consecutive positions (finding 171), so both occurrences survive and both
# count: `sum` 4.0 and `average` 2.0 rather than 2.0 and 2.0. z4 uses `bird`, a
# term no other row queries, so adding it moves none of the committed fixtures.
capplsz plsz_dup_sum     "select?q=%7B%21payload_score%20f%3Dboost_term%20v%3D%22bird%22%20func%3Dsum%7D&$PLSZ_TAIL"
capplsz plsz_dup_average "select?q=%7B%21payload_score%20f%3Dboost_term%20v%3D%22bird%22%20func%3Daverage%7D&$PLSZ_TAIL"

if want_any '^plsz_'; then
  release "$PLSZ_CONTAINER" "payload-free core '$PLSZ_CORE'"
fi

# --- json.facet / JSON Facet API (issue #343) -------------------------------
# Appended block; own Solr 9 core and port. Captures the JSON Facet API shapes
# `search_api_solr` 4.4.0 actually sends on its admin diagnostics screens
# (finding 132): a bare aggregation string, a `type: terms` facet, and terms
# nesting via the `facet` key, up to the four-level
# `doGetMaxDocumentVersions()` shape.
#
# Two deliberate parallel captures for every aggregation:
#   * `max(_version_)` -- the client's real probe. Its VALUES are Solr's opaque
#     update-log versions and differ on every capture, so these fixtures are
#     ground truth for the response SHAPE only.
#   * `max(popularity)` -- the same shapes over a deterministic pint field, so
#     the aggregation's VALUES are assertable ground truth too. Without this
#     pair a test could only check that a number appeared, not that it was the
#     right number.
#
# `json.facet` values are JSON objects, so every capture goes through
# `curl -G --data-urlencode` and records `%{url_effective}`'s query -- writing
# raw braces and quotes into a manifest row would not survive a verbatim GET.
# Rows land in `manifest-errors.tsv` because this is not the `content` core.
JF343_CONTAINER=wayfinder-solr-343
JF343_SOLR=http://localhost:9081/solr
JF343_CORE=jsonfacet343
if want_any '^jf343_'; then
  if ! docker ps --format '{{.Names}}' | grep -qx "$JF343_CONTAINER"; then
    docker rm -f "$JF343_CONTAINER" >/dev/null 2>&1 || true
    docker run -d --name "$JF343_CONTAINER" -p 9081:8983 \
      solr:9 solr-precreate "$JF343_CORE" >/dev/null
  fi
  echo -n "waiting for json-facet solr"
  for _ in $(seq 60); do
    if curl -sf "$JF343_SOLR/$JF343_CORE/admin/ping?wt=json" >/dev/null 2>&1; then echo " ok"; break; fi
    echo -n "."; sleep 1
  done
  # `_version_` comes from Solr's default schema and is not configured here.
  # The three string fields are the exact ones the client facets on.
  curl -s "$JF343_SOLR/$JF343_CORE/schema" -H 'Content-Type: application/json' -d '{
    "add-field": [
      {"name":"hash",                     "type":"string", "indexed":true, "stored":true, "docValues":true},
      {"name":"index_id",                 "type":"string", "indexed":true, "stored":true, "docValues":true},
      {"name":"ss_search_api_datasource", "type":"string", "indexed":true, "stored":true, "docValues":true},
      {"name":"popularity",               "type":"pint",   "indexed":true, "stored":true, "docValues":true},
      {"name":"body",                     "type":"text_en","indexed":true, "stored":true}
    ]
  }' >/dev/null
  # Corpus shaped so every nesting level has distinguishable counts and a
  # distinguishable max: siteA has two indexes, index_a has two datasources,
  # and `entity:node` under index_a has two docs whose popularity max (30) is
  # neither the global max (60) nor its own bucket's first value (10).
  # `jf6` carries no `hash`/`index_id` at all, so it is the doc the client's
  # `fq=+hash:* +index_id:*` excludes and the doc that makes a terms facet's
  # default `mincount: 1` observable (no empty bucket for it).
  curl -sf "$JF343_SOLR/$JF343_CORE/update?commit=true" -H 'Content-Type: application/json' -d '[
    {"id":"jf1","hash":"siteA","index_id":"index_a","ss_search_api_datasource":"entity:node","popularity":10,"body":"alpha"},
    {"id":"jf2","hash":"siteA","index_id":"index_a","ss_search_api_datasource":"entity:node","popularity":30,"body":"beta"},
    {"id":"jf3","hash":"siteA","index_id":"index_a","ss_search_api_datasource":"entity:user","popularity":20,"body":"gamma"},
    {"id":"jf4","hash":"siteA","index_id":"index_b","ss_search_api_datasource":"entity:node","popularity":40,"body":"delta"},
    {"id":"jf5","hash":"siteB","index_id":"index_c","ss_search_api_datasource":"entity:node","popularity":50,"body":"epsilon"},
    {"id":"jf6","popularity":60,"body":"zeta orphan"}
  ]' >/dev/null
fi

capjf343() {  # capjf343 <name> <json.facet value> [extra raw query params]
  local name=$1 jf=$2 extra=${3:-}
  want "$name" || return 0
  local url
  url=$(curl -sg -G "$JF343_SOLR/$JF343_CORE/select" \
    --data-urlencode "json.facet=$jf" \
    ${extra:+--data "$extra"} \
    -o "$OUT/$name.json" -w '%{http_code}\t%{url_effective}')
  local status=${url%%$'\t'*} effective=${url#*$'\t'}
  printf '%s\t%s\t%s\t%s\t%s\t%s\n' \
    "$name" "$status" GET "$JF343_CORE/select?${effective#*\?}" "" "$JF343_SOLR" \
    >> "$MANIFEST_ERRORS"
}

JF343_TAIL='q=*:*&rows=0&wt=json'
# The client's own filter query, verbatim from SearchApiSolrBackend.php:4900-4903.
# Pre-encoded: `--data` with `-G` appends the string to the query verbatim, so
# the braces, the `+` required-clause operators and the space must already be
# escaped or Jetty rejects the line.
JF343_FQ='q=*:*&rows=1&fl=id&wt=json&fq=%7B!key%3Dsearch_api%7D%2Bhash%3A*%20%2Bindex_id%3A*'

# 1. Aggregation only, deterministic field: pins the implicit top-level `count`
#    (which the client reads unguarded) plus a bare-string aggregation.
capjf343 jf343_agg_max            '{"maxPopularity":"max(popularity)"}'                     "$JF343_TAIL"
# 2. The client's literal fallback shape: rows=1 and omitHeader=false (the
#    SOLR-13509 workaround at PHP 4943-4950). Shape-only ground truth.
capjf343 jf343_agg_max_version    '{"maxVersion":"max(_version_)"}'                         'q=*:*&rows=1&wt=json&omitHeader=false'
# 3. Single terms facet, `limit: -1` exactly as the client sends it. Proves
#    unlimited buckets and that `jf6` produces no bucket (default mincount 1).
capjf343 jf343_terms              '{"siteHashes":{"limit":-1,"field":"hash","type":"terms"}}' "$JF343_TAIL"
# 4. `limit: 2` -- the truncation the client never asks for but the parser must
#    honour, and the control proving `-1` above is really unlimited.
capjf343 jf343_terms_limit        '{"siteHashes":{"limit":2,"field":"index_id","type":"terms"}}' "$JF343_TAIL"
# 5. doDocumentCounts()'s Drupal shape (PHP 4914-4926): two-level terms nesting
#    under the `facet` key, sub-buckets inline in each parent bucket.
capjf343 jf343_terms_nested       '{"siteHashes":{"limit":-1,"field":"hash","type":"terms","facet":{"numDocsPerIndex":{"limit":-1,"field":"index_id","type":"terms"}}}}' "$JF343_TAIL"
# 6. The deepest shape the client ever sends (doGetMaxDocumentVersions(), PHP
#    5052-5082): top-level aggregation + three terms levels + a leaf
#    aggregation. `_version_` variant is shape-only ground truth.
capjf343 jf343_deep_version       '{"maxVersion":"max(_version_)","siteHashes":{"limit":-1,"field":"hash","type":"terms","facet":{"indexes":{"limit":-1,"field":"index_id","type":"terms","facet":{"dataSources":{"limit":-1,"field":"ss_search_api_datasource","type":"terms","facet":{"maxVersionPerDataSource":"max(_version_)"}}}}}}}' "$JF343_FQ"
# 7. Same topology over `popularity`, so every bucket's aggregate value is
#    assertable: entity:node under index_a must be 30, not 10 and not 60.
capjf343 jf343_deep_max           '{"maxPopularity":"max(popularity)","siteHashes":{"limit":-1,"field":"hash","type":"terms","facet":{"indexes":{"limit":-1,"field":"index_id","type":"terms","facet":{"dataSources":{"limit":-1,"field":"ss_search_api_datasource","type":"terms","facet":{"maxPopularityPerDataSource":"max(popularity)"}}}}}}}' "$JF343_TAIL"
# 8. The client's `fq` applied to a terms facet: `jf6` is excluded, so this is
#    the evidence that json facets count the filtered set, not the whole index.
capjf343 jf343_terms_fq           '{"siteHashes":{"limit":-1,"field":"hash","type":"terms"}}' "$JF343_FQ"
# 9. A restricting `q` rather than an `fq`, same purpose for the main query.
capjf343 jf343_terms_q            '{"siteHashes":{"limit":-1,"field":"hash","type":"terms"}}' 'q=body%3Aalpha%20OR%20body%3Abeta%20OR%20body%3Azeta&rows=0&wt=json&df=body'
# 10. json.facet ALONGSIDE classic faceting: pins whether `facets` and
#     `facet_counts` coexist and their top-level envelope order, which decides
#     where the new block is inserted in the response map.
capjf343 jf343_with_classic       '{"maxPopularity":"max(popularity)"}'                     'q=*:*&rows=0&wt=json&facet=true&facet.field=hash'
# 11. Mincount made explicit: `mincount: 0` is unevidenced client-side but is
#     the control that proves the default really is 1 in capture 3.
capjf343 jf343_terms_mincount0    '{"siteHashes":{"limit":-1,"field":"hash","type":"terms","mincount":0}}' "$JF343_TAIL"

# Error shapes: what Solr does with a json.facet Wayfinder must also reject.
# 12. Malformed JSON in the param value.
capjf343 jf343_err_bad_json       '{"siteHashes":{"field":'                                 "$JF343_TAIL"
# 13. Unknown facet type.
capjf343 jf343_err_bad_type       '{"siteHashes":{"type":"nosuchtype","field":"hash"}}'     "$JF343_TAIL"
# 14. Unknown aggregation function.
capjf343 jf343_err_bad_func       '{"x":"nosuchfunc(popularity)"}'                          "$JF343_TAIL"
# 15. Terms facet on an undefined field.
capjf343 jf343_err_unknown_field  '{"x":{"type":"terms","field":"no_such_field"}}'           "$JF343_TAIL"
# 16. Terms facet naming a field with no docValues (`body` is text_en, indexed
#     and stored but not docValues) -- the JSON-facet analogue of finding 105's
#     classic-facet 400.
capjf343 jf343_err_no_docvalues   '{"x":{"type":"terms","field":"body"}}'                    "$JF343_TAIL"
# 17. Aggregation over a text field.
capjf343 jf343_err_agg_text       '{"x":"max(body)"}'                                        "$JF343_TAIL"

# 18. `facets` alongside BOTH classic faceting and `stats`: pins the full
#     top-level envelope order. Capture 10 established `facet_counts` before
#     `facets`; this settles where `stats` falls, which is the last unknown for
#     the response-assembly insert point.
capjf343 jf343_with_classic_stats '{"maxPopularity":"max(popularity)"}'                      'q=*:*&rows=0&wt=json&facet=true&facet.field=hash&stats=true&stats.field=popularity'
# 19. An empty JSON facet object: does the implicit `count` still appear, and is
#     `facets` emitted at all? The parser needs a defined answer for the
#     degenerate input.
capjf343 jf343_empty_object       '{}'                                                       "$JF343_TAIL"
# 20. `sort` inside the JSON object. Unevidenced client-side, but capture 3's
#     count-desc bucket order is otherwise indistinguishable from insertion
#     order on this corpus; this is the control that names the default.
capjf343 jf343_terms_sort_index    '{"siteHashes":{"limit":-1,"field":"index_id","type":"terms","sort":"index asc"}}' "$JF343_TAIL"

if want_any '^jf343_'; then
  release "$JF343_CONTAINER" "json-facet core '$JF343_CORE'"
fi

DR341_TAIL='fl=id&sort=id%20asc&rows=20&wt=json'
# The query interval every predicate fixture below uses, so the three ops are
# directly comparable on one corpus: May 1 -- Jul 1 2020.
DR341_Q='%5B2020-05-01T00%3A00%3A00Z%20TO%202020-07-01T00%3A00%3A00Z%5D'

# Stored values round-trip VERBATIM -- Solr does not normalise "2020" into an
# expanded interval, and drm_x keeps both members in input order. This is the
# fixture the response writer is derived from.
capdr341 dr341_roundtrip "$DR341_CORE/select?q=*:*&fl=id,drs_x,drm_x&sort=id%20asc&rows=20&wt=json"

# Plain `field:[a TO b]` and an explicit `op=Intersects` are the SAME query:
# both -> d1,d2,d3,d4,d7. d5 (ends 2019) and d6 (starts 2021) are the docs that
# make this a real interval test rather than a bounds comparison.
capdr341 dr341_intersects_plain "$DR341_CORE/select?q=drs_x%3A$DR341_Q&$DR341_TAIL"
capdr341 dr341_op_intersects    "$DR341_CORE/select?q=%7B%21field%20f%3Ddrs_x%20op%3DIntersects%7D$DR341_Q&$DR341_TAIL"
# `{!field f=...}` with NO op at all also defaults to Intersects.
capdr341 dr341_op_default       "$DR341_CORE/select?q=%7B%21field%20f%3Ddrs_x%7D2020-06&$DR341_TAIL"
# Contains: the doc's interval must contain the whole query interval ->
# d1 (all 2020), d3 (Mar-Sep), d7 (open). Within: the doc's interval must fit
# inside the query -> d2 (June), d4 (one second). The two are not complements.
capdr341 dr341_op_contains      "$DR341_CORE/select?q=%7B%21field%20f%3Ddrs_x%20op%3DContains%7D$DR341_Q&$DR341_TAIL"
capdr341 dr341_op_within        "$DR341_CORE/select?q=%7B%21field%20f%3Ddrs_x%20op%3DWithin%7D$DR341_Q&$DR341_TAIL"
# `IsWithin` is an accepted alias of `Within` (identical result), and the op
# value is matched case-INSENSITIVELY (`contains` == `Contains`).
capdr341 dr341_op_iswithin      "$DR341_CORE/select?q=%7B%21field%20f%3Ddrs_x%20op%3DIsWithin%7D$DR341_Q&$DR341_TAIL"
capdr341 dr341_op_lowercase     "$DR341_CORE/select?q=%7B%21field%20f%3Ddrs_x%20op%3Dcontains%7D$DR341_Q&$DR341_TAIL"

# A bare date literal is a range at its own precision, so `drs_x:2020-06` and
# `drs_x:2020` are interval queries, not term queries -- both intersect the same
# five docs as the explicit May-Jul range above.
capdr341 dr341_single_year  "$DR341_CORE/select?q=drs_x%3A2020&$DR341_TAIL"
capdr341 dr341_single_month "$DR341_CORE/select?q=drs_x%3A2020-06&$DR341_TAIL"
# `[* TO *]` is every doc that HAS the field: d1-d7, and not d8 (drm_x only).
capdr341 dr341_star_both    "$DR341_CORE/select?q=drs_x%3A%5B*%20TO%20*%5D&$DR341_TAIL"
# The multiValued field: a doc matches if ANY of its intervals does.
capdr341 dr341_multi_intersects "$DR341_CORE/select?q=drm_x%3A2022-05&$DR341_TAIL"
capdr341 dr341_multi_contains   "$DR341_CORE/select?q=%7B%21field%20f%3Ddrm_x%20op%3DContains%7D2022-05&$DR341_TAIL"

# Exclusive-brace syntax is ACCEPTED and IGNORED: `{a TO b}` returns exactly
# what `[a TO b]` does. DateRangeField parses the interval string itself and has
# no notion of an exclusive endpoint, so this is a real trap for any
# implementation that routes the query through a lucene range parser first.
capdr341 dr341_excl_braces "$DR341_CORE/select?q=drs_x%3A%7B2020-05-01T00%3A00%3A00Z%20TO%202020-07-01T00%3A00%3A00Z%7D&$DR341_TAIL"

# Millisecond-resolution, end-INCLUSIVE precision expansion. d2 is "2020-06":
# a Within query ending at 23:59:59.999 on Jun 30 still contains it, one
# millisecond earlier (.998) does not. This pair is what pins the expansion to
# ms rather than seconds.
capdr341 dr341_within_ms_exact "$DR341_CORE/select?q=%7B%21field%20f%3Ddrs_x%20op%3DWithin%7D%5B2020-06-01T00%3A00%3A00Z%20TO%202020-06-30T23%3A59%3A59.999Z%5D&$DR341_TAIL"
capdr341 dr341_within_ms_short "$DR341_CORE/select?q=%7B%21field%20f%3Ddrs_x%20op%3DWithin%7D%5B2020-06-01T00%3A00%3A00Z%20TO%202020-06-30T23%3A59%3A59.998Z%5D&$DR341_TAIL"
# The same rule applied to an interval ENDPOINT: d5 ends "2019-12-31T23:59:59Z",
# which is that whole second, so a query starting one millisecond past it still
# intersects d5. Both fixtures -> d1,d5,d7.
capdr341 dr341_touch_endpoint "$DR341_CORE/select?q=drs_x%3A%5B2019-12-31T23%3A59%3A59Z%20TO%202020-01-01T00%3A00%3A00Z%5D&$DR341_TAIL"
capdr341 dr341_touch_past_ms  "$DR341_CORE/select?q=drs_x%3A%5B2019-12-31T23%3A59%3A59.001Z%20TO%202020-01-01T00%3A00%3A00Z%5D&$DR341_TAIL"

# --- multiValued: set operations on the UNION of the doc's intervals --------
# These five exist because dr341_multi_intersects/_contains above cannot tell a
# correct implementation from a broken one: d8 was the only doc with drm_x, so a
# single expected id proves nothing about HOW it matched.
#
# The rule every one of them is consistent with is that a multiValued
# DateRangeField behaves as ONE point set --- the union of its members, holes
# included --- and each op is a set relation against the query interval:
#   Intersects  union intersects query
#   Contains    union CONTAINS query   (so a hole in the union defeats it)
#   Within      union is WITHIN query  (so EVERY member must fit)
# For a single-valued field all three collapse to the obvious per-interval test,
# which is why only drm_x can distinguish them.
#
# d8 = {"2020", "2022-05"} (a hole covering all of 2021), d9 = {"2020"}.
#
#   _gap          Intersects [2021-01 TO 2021-06] -> NEITHER. Lands in d8's
#                 hole. An implementation that collapses the field to one span
#                 (min start .. max end = 2020-01-01 .. 2022-05-31) matches d8.
#                 That collapse is the tempting shortcut, because a columnar
#                 store does not record which start pairs with which end.
#   _no_contains  Contains [2020-06 TO 2022-01] -> NEITHER. Spans d8's hole, so
#                 the union does not cover it; the merged span would.
#   _within_one   Within [2020-01-01 TO 2020-12-31.999] -> d9 ONLY. This is the
#                 decisive one: d8's "2020" member fits perfectly, so an
#                 "any member is within" reading matches d8 too. Real Solr does
#                 not --- d8's 2022-05 member is outside the query, and Within
#                 is about the whole union.
#   _within_both  Within [2019 TO 2023.999] -> BOTH. The same query widened
#                 until d8's union does fit, which is what makes _within_one's
#                 exclusion of d8 attributable to the union rule rather than to
#                 multiValued fields being broken for Within generally.
#   _contains_one Contains 2020-06 -> BOTH. A query inside a single member DOES
#                 match, confirming Contains is not itself demanding all members.
capdr341 dr341_multi_gap         "$DR341_CORE/select?q=drm_x%3A%5B2021-01%20TO%202021-06%5D&$DR341_TAIL"
capdr341 dr341_multi_no_contains "$DR341_CORE/select?q=%7B%21field%20f%3Ddrm_x%20op%3DContains%7D%5B2020-06%20TO%202022-01%5D&$DR341_TAIL"
capdr341 dr341_multi_within_one  "$DR341_CORE/select?q=%7B%21field%20f%3Ddrm_x%20op%3DWithin%7D%5B2020-01-01T00%3A00%3A00Z%20TO%202020-12-31T23%3A59%3A59.999Z%5D&$DR341_TAIL"
capdr341 dr341_multi_within_both "$DR341_CORE/select?q=%7B%21field%20f%3Ddrm_x%20op%3DWithin%7D%5B2019-01-01T00%3A00%3A00Z%20TO%202023-12-31T23%3A59%3A59.999Z%5D&$DR341_TAIL"
capdr341 dr341_multi_contains_one "$DR341_CORE/select?q=%7B%21field%20f%3Ddrm_x%20op%3DContains%7D2020-06&$DR341_TAIL"

# Date math resolves against NOW, so a committed fixture is only ground truth
# for as long as its result set does not depend on a boundary NOW has crossed.
# Both of these are chosen so that window is longer than the project:
#   dr341_datemath_year `[NOW/YEAR TO NOW/YEAR+1YEAR]` -> d6 ([2021 TO *]) and
#     d7 ([* TO *]). The corpus has no interval starting between 2023 and 2100,
#     so this holds for every NOW in that span.
#   dr341_datemath_now `[NOW-100YEARS TO NOW]` -> all 7. Holds until the lower
#     bound passes d5's end (2019-12-31), i.e. until the year 2119.
# Do NOT add a date-math fixture whose result set turns over sooner: the failure
# mode is a fixture that silently stops being ground truth years from now, with
# nothing pointing at the cause.
capdr341 dr341_datemath_year  "$DR341_CORE/select?q=drs_x%3A%5BNOW%2FYEAR%20TO%20NOW%2FYEAR%2B1YEAR%5D&$DR341_TAIL"
capdr341 dr341_datemath_now   "$DR341_CORE/select?q=drs_x%3A%5BNOW-100YEARS%20TO%20NOW%5D&$DR341_TAIL"

# facet.field on a DateRangeField is NOT an error -- it returns an EMPTY bucket
# list with HTTP 200. Sorting and stats DO 400, each with its own message.
capdr341 dr341_facet_empty "$DR341_CORE/select?q=*:*&rows=0&facet=true&facet.field=drs_x&wt=json"
capdr341 dr341_err_sort    "$DR341_CORE/select?q=*:*&fl=id&sort=drs_x%20asc&rows=20&wt=json"
capdr341 dr341_err_stats   "$DR341_CORE/select?q=*:*&rows=0&stats=true&stats.field=drs_x&wt=json"

# Error surface, and the 400-vs-500 split is ground truth, not an accident:
# a value Solr cannot PARSE is a 400; a structurally valid query asking for an
# operation DateRangeField does not implement is a 500.
capdr341 dr341_err_bad_date  "$DR341_CORE/select?q=drs_x%3A%5B2020-13%20TO%202021%5D&fl=id&wt=json"
capdr341 dr341_err_bad_math  "$DR341_CORE/select?q=drs_x%3A%5BNOW%2FBOGUS%20TO%20NOW%5D&fl=id&wt=json"
capdr341 dr341_err_reversed  "$DR341_CORE/select?q=drs_x%3A%5B2021%20TO%202020%5D&fl=id&wt=json"
capdr341 dr341_err_bad_op    "$DR341_CORE/select?q=%7B%21field%20f%3Ddrs_x%20op%3DBogus%7D%5B2020%20TO%202021%5D&fl=id&wt=json"
capdr341 dr341_err_disjoint  "$DR341_CORE/select?q=%7B%21field%20f%3Ddrs_x%20op%3DIsDisjointTo%7D$DR341_Q&fl=id&wt=json"
capdr341 dr341_err_overlaps  "$DR341_CORE/select?q=%7B%21field%20f%3Ddrs_x%20op%3DOverlaps%7D$DR341_Q&fl=id&wt=json"
capdr341 dr341_err_equals    "$DR341_CORE/select?q=%7B%21field%20f%3Ddrs_x%20op%3DEquals%7D$DR341_Q&fl=id&wt=json"

# The type's own declaration as /schema/fieldtypes reports it, which is what
# `field_class_for_builtin` is derived from.
capdr341 dr341_fieldtypes "$DR341_CORE/schema/fieldtypes?wt=json"

if want_any '^dr341_'; then
  release "$DR341_CONTAINER" "date-range core '$DR341_CORE'"
fi
