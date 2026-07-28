#!/usr/bin/env bash
# Capture reference /select responses from a real Solr for the tracer-bullet schema.
# Output: solr-ref/responses/*.json + manifest.tsv (query -> file)
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OUT="$HERE/responses"
CORE=content
SOLR=http://localhost:8983/solr
CONTAINER=wayfinder-solr-ref

rm -rf "$OUT"; mkdir -p "$OUT"
: > "$HERE/manifest.tsv"

# --- Solr up ---------------------------------------------------------------
if ! docker ps --format '{{.Names}}' | grep -qx "$CONTAINER"; then
  docker rm -f "$CONTAINER" >/dev/null 2>&1 || true
  docker run -d --name "$CONTAINER" -p 8983:8983 solr:9 solr-precreate "$CORE" >/dev/null
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
  ]
}' >/dev/null

# --- Corpus ----------------------------------------------------------------
curl -sf "$SOLR/$CORE/update?commit=true" -H 'Content-Type: application/json' -d '[
  {"id":"doc1","body":"the quick brown fox jumps over the lazy dog","category":["animals","classic"]},
  {"id":"doc2","body":"a lazy afternoon in the garden","category":["garden"]},
  {"id":"doc3","body":"quick thinking saves the day","category":["misc","classic"]},
  {"id":"doc4","body":"dogs and cats living together","category":["animals"]},
  {"id":"doc5","body":"nothing much here at all"}
]' >/dev/null

# --- Capture ---------------------------------------------------------------
cap() {  # cap <name> <path-with-query>
  local name=$1 path=$2
  # -g: disable URL globbing, or curl chokes on '[' in the bad-syntax fixture
  curl -sg "$SOLR/$CORE/$path" -o "$OUT/$name.json" -w '%{http_code}' > "$OUT/$name.status"
  printf '%s\t%s\t%s\n' "$name" "$(cat "$OUT/$name.status")" "$path" >> "$HERE/manifest.tsv"
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
  if [ -n "$body" ]; then
    curl -sg -X "$method" "$SOLR/$suffix" -H 'Content-Type: application/json' -d "$body" \
      -o "$OUT/$name.json" -w '%{http_code}' > "$OUT/$name.status"
  else
    curl -sg -X "$method" "$SOLR/$suffix" \
      -o "$OUT/$name.json" -w '%{http_code}' > "$OUT/$name.status"
  fi
  printf '%s\t%s\t%s\t%s\t%s\n' \
    "$name" "$(cat "$OUT/$name.status")" "$method" "$suffix" "$body" \
    >> "$HERE/manifest-errors.tsv"
  rm -f "$OUT/$name.status"
}
: > "$HERE/manifest-errors.tsv"

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
  curl -sg "$base/$core/$path" -H 'Content-Type: application/json' -d "$body" \
    -o "$OUT/$name.json" -w '%{http_code}' > "$OUT/$name.status"
  printf '%s\t%s\t%s\t%s\t%s\t%s\n' \
    "$name" "$(cat "$OUT/$name.status")" POST "$core/$path" "$body" "$base" \
    >> "$HERE/manifest-errors.tsv"
  rm -f "$OUT/$name.status"
}

# Schemaless side, on the main container. `commit=false` and a delete-by-id
# afterwards keep the 5-doc reference corpus intact for every other fixture.
cap_post update_unknown_field_schemaless 'update?commit=true' \
  '[{"id":"probe_unknown_field","body":"probe","nosuchfield":"x"}]'
curl -s "$SOLR/$CORE/update?commit=true" -H 'Content-Type: application/json' \
  -d '{"delete":{"id":"probe_unknown_field"}}' >/dev/null

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

echo
column -t -s $'\t' "$HERE/manifest.tsv"
echo
column -t -s $'\t' "$HERE/manifest-errors.tsv"
echo
echo "captured $(wc -l < "$HERE/manifest.tsv" | tr -d ' ') responses -> $OUT"
echo "solr still running as '$CONTAINER' (docker rm -f $CONTAINER to stop)"
echo "strict solr still running as '$STRICT_CONTAINER' (docker rm -f $STRICT_CONTAINER to stop)"
