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
column -t -s $'\t' "$HERE/manifest.tsv"
echo
column -t -s $'\t' "$HERE/manifest-errors.tsv"
echo
echo "captured $(wc -l < "$HERE/manifest.tsv" | tr -d ' ') responses -> $OUT"
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
column -t -s $'\t' "$HERE/manifest.tsv"
echo
column -t -s $'\t' "$HERE/manifest-errors.tsv"
echo
echo "captured $(wc -l < "$HERE/manifest.tsv" | tr -d ' ') manifest.tsv rows -> $OUT"
echo "range-facet core '$RANGE_CORE' left in place on '$CONTAINER'"
