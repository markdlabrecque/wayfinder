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
  curl -sg "$KEYORDER_SOLR/$KEYORDER_CORE/$suffix" \
    -o "$OUT/$name.json" -w '%{http_code}' > "$OUT/$name.status"
  printf '%s\t%s\t%s\t%s\t%s\t%s\n' \
    "$name" "$(cat "$OUT/$name.status")" GET "$KEYORDER_CORE/$suffix" "" "$KEYORDER_SOLR" \
    >> "$HERE/manifest-errors.tsv"
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
  curl -sg "$FACET_SOLR/$suffix" -o "$OUT/$name.json" -w '%{http_code}' > "$OUT/$name.status"
  printf '%s\t%s\t%s\t%s\t%s\t%s\n' \
    "$name" "$(cat "$OUT/$name.status")" GET "$suffix" "" "$FACET_SOLR" \
    >> "$HERE/manifest-errors.tsv"
  rm -f "$OUT/$name.status"
}

capk keyorder_range_wide_map \
  'select?q=*:*&rows=0&facet=true&facet.range=views&facet.range.start=0&facet.range.end=200&facet.range.gap=10&json.nl=map&wt=json'
capk keyorder_facet_field_map \
  'select?q=*:*&rows=0&facet=true&facet.field=tag&json.nl=map&wt=json'
capk keyorder_facet_field_map_index \
  'select?q=*:*&rows=0&facet=true&facet.field=tag&facet.sort=index&json.nl=map&wt=json'

echo "key-order core '$KEYORDER_CORE' left in place on '$KEYORDER_CONTAINER' (port 8986)"

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
  curl -sg "$SORTDEBT_SOLR/$SORTDEBT_CORE/$suffix" \
    -o "$OUT/$name.json" -w '%{http_code}' > "$OUT/$name.status"
  printf '%s\t%s\t%s\t%s\t%s\t%s\n' \
    "$name" "$(cat "$OUT/$name.status")" GET "$SORTDEBT_CORE/$suffix" "" "$SORTDEBT_SOLR" \
    >> "$HERE/manifest-errors.tsv"
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
column -t -s $'\t' "$HERE/manifest-errors.tsv"
echo
echo "numeric/date facet.field core '$FACET_CORE' left in place on '$FACET_CONTAINER'"
echo "  (docker rm -f $FACET_CONTAINER to stop)"
echo "sort-debt core '$SORTDEBT_CORE' left in place on '$SORTDEBT_CONTAINER' (port 8987)"
echo "  (docker rm -f $SORTDEBT_CONTAINER to stop)"

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
  curl -sg "$DEBT_SOLR/$suffix" -o "$OUT/$name.json" -w '%{http_code}' > "$OUT/$name.status"
  printf '%s\t%s\t%s\t%s\t%s\t%s\n' \
    "$name" "$(cat "$OUT/$name.status")" GET "$suffix" "" "$DEBT_SOLR" \
    >> "$HERE/manifest-errors.tsv"
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

echo "facet-debt core '$DEBT_CORE' left in place on '$DEBT_CONTAINER' (port 8988)"
echo "  (docker rm -f $DEBT_CONTAINER to stop)"

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
  curl -sg "$UPDATE9_SOLR/$suffix" -H 'Content-Type: application/json' -d "$body" \
    -o "$OUT/$name.json" -w '%{http_code}' > "$OUT/$name.status"
  printf '%s\t%s\t%s\t%s\t%s\t%s\n' \
    "$name" "$(cat "$OUT/$name.status")" POST "$suffix" "$body" "$UPDATE9_SOLR" \
    >> "$HERE/manifest-errors.tsv"
  rm -f "$OUT/$name.status"
}
# Arbitrary-method helper (GET /update, DELETE /admin/ping, unknown core),
# same 6-column contract, empty body column.
capu() {  # capu <name> <method> <url-after-/solr/>
  local name=$1 method=$2 suffix=$3
  curl -sg -X "$method" "$UPDATE9_SOLR/$suffix" \
    -o "$OUT/$name.json" -w '%{http_code}' > "$OUT/$name.status"
  printf '%s\t%s\t%s\t%s\t%s\t%s\n' \
    "$name" "$(cat "$OUT/$name.status")" "$method" "$suffix" "" "$UPDATE9_SOLR" \
    >> "$HERE/manifest-errors.tsv"
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

echo "update-pipeline core '$UPDATE9_CORE' left in place on '$UPDATE9_CONTAINER' (port 8989)"
echo "  (docker rm -f $UPDATE9_CONTAINER to stop)"

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
  curl -sg "$WILDCARD_SOLR/$WILDCARD_CORE/$path" -o "$OUT/$name.json" -w '%{http_code}' > "$OUT/$name.status"
  printf '%s\t%s\t%s\n' "$name" "$(cat "$OUT/$name.status")" "$path" >> "$HERE/manifest.tsv"
  rm -f "$OUT/$name.status"
}
capw select_wildcard_and_term   'select?q=*:*+AND+lazy&df=body&fl=id,body&wt=json'
capw select_wildcard_or_term    'select?q=lazy+OR+*:*&df=body&fl=id,body&wt=json'
capw select_wildcard_minus_term 'select?q=*:*+-lazy&df=body&fl=id,body&wt=json'
echo "wildcard-panic core '$WILDCARD_CORE' left in place on '$WILDCARD_CONTAINER' (port 8990)"
echo "  (docker rm -f $WILDCARD_CONTAINER to stop)"

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
  curl -sg "$STATS_SOLR/$suffix" -o "$OUT/$name.json" -w '%{http_code}' > "$OUT/$name.status"
  printf '%s\t%s\t%s\t%s\t%s\t%s\n' \
    "$name" "$(cat "$OUT/$name.status")" GET "$suffix" "" "$STATS_SOLR" \
    >> "$HERE/manifest-errors.tsv"
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

echo "stats core '$STATS_CORE' left in place on '$STATS_CONTAINER' (port 8992)"
echo "  (docker rm -f $STATS_CONTAINER to stop)"

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
  curl -sg "$HL_SOLR/$HL_CORE/$path" -o "$OUT/$name.json" -w '%{http_code}' > "$OUT/$name.status"
  printf '%s\t%s\t%s\n' "$name" "$(cat "$OUT/$name.status")" "$path" >> "$HERE/manifest.tsv"
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

echo "highlighting core '$HL_CORE' left in place on '$HL_CONTAINER' (port 8991)"
echo "  (docker rm -f $HL_CONTAINER to stop)"
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
  curl -sg "$SOLR/facets/$suffix" -o "$OUT/$name.json" -w '%{http_code}' > "$OUT/$name.status"
  printf '%s\t%s\t%s\t%s\t%s\t%s\n' \
    "$name" "$(cat "$OUT/$name.status")" GET "facets/$suffix" "" "$SOLR" \
    >> "$HERE/manifest-errors.tsv"
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
  curl -sg "$MLT_SOLR/$MLT_CORE/$path" -o "$OUT/$name.json" -w '%{http_code}' > "$OUT/$name.status"
  printf '%s\t%s\t%s\n' "$name" "$(cat "$OUT/$name.status")" "$path" >> "$HERE/manifest.tsv"
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
echo "mlt core '$MLT_CORE' left in place on '$MLT_CONTAINER' (port 8993)"
echo "  (docker rm -f $MLT_CONTAINER to stop)"

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
  curl -sg "$EDISMAX_SOLR/$EDISMAX_CORE/$path" -o "$OUT/$name.json" -w '%{http_code}' > "$OUT/$name.status"
  printf '%s\t%s\t%s\n' "$name" "$(cat "$OUT/$name.status")" "$path" >> "$HERE/manifest.tsv"
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
# above lived only in prose (finding 85, tests/edismax.rs comments) -- this is
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
echo "edismax core '$EDISMAX_CORE' left in place on '$EDISMAX_CONTAINER' (port 8994)"
echo "  (docker rm -f $EDISMAX_CONTAINER to stop)"

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
  curl -sg "$VERSION_SOLR/$suffix" -o "$OUT/$name.json" -w '%{http_code}' > "$OUT/$name.status"
  printf '%s\t%s\t%s\t%s\t%s\t%s\n' \
    "$name" "$(cat "$OUT/$name.status")" GET "$suffix" "" "$VERSION_SOLR" \
    >> "$HERE/manifest-errors.tsv"
  rm -f "$OUT/$name.status"
}
capv stats_version_max "$VERSION_CORE/select?q=*:*&rows=0&stats=true&stats.field=_version_&function=max(_version_)&wt=json"
echo "version-field core '$VERSION_CORE' left in place on '$VERSION_CONTAINER' (port 8999)"
echo "  (docker rm -f $VERSION_CONTAINER to stop)"

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
  curl -sg "$FRAGSIZE_SOLR/$suffix" -o "$OUT/$name.json" -w '%{http_code}' > "$OUT/$name.status"
  printf '%s\t%s\t%s\t%s\t%s\t%s\n' \
    "$name" "$(cat "$OUT/$name.status")" GET "$suffix" "" "$FRAGSIZE_SOLR" \
    >> "$HERE/manifest-errors.tsv"
  rm -f "$OUT/$name.status"
}
capf hl_fragsize_zero_whole_field "$FRAGSIZE_CORE/select?q=body:quick&hl=true&hl.fl=body&hl.fragsize=0&wt=json"
capf hl_fragsize_zero_whole_field_method_original "$FRAGSIZE_CORE/select?q=body:quick&hl=true&hl.fl=body&hl.method=original&hl.fragsize=0&wt=json"
capf hl_fragsize_small_truncated "$FRAGSIZE_CORE/select?q=body:quick&hl=true&hl.fl=body&hl.method=original&hl.fragsize=40&wt=json"
echo "fragsize core '$FRAGSIZE_CORE' left in place on '$FRAGSIZE_CONTAINER' (port 8995)"
echo "  (docker rm -f $FRAGSIZE_CONTAINER to stop)"

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
