# Getting started

Run these commands from the repository root. This is one local process serving
one `getting-started` core from one schema and one data directory on one
loopback listener. The JSON routes are bounded and Solr-shaped for supported
clients; this guide does not promise Solr parity.

## Build or obtain the binary

Build the checked-out source, or set `WAYFINDER` to an obtained compatible
binary. The commands below use the canonical files in this directory exactly.

```sh
cargo build --locked --release
WAYFINDER=target/release/wayfinder
SCHEMA=manual/getting-started/schema.toml
CORPUS=manual/getting-started/corpus.json
CORE=getting-started
LISTENER=127.0.0.1:8983
BASE=http://$LISTENER/wayfinder/$CORE
DATA_DIR=$(mktemp -d "${TMPDIR:-/tmp}/wayfinder-getting-started.XXXXXX")
LOG=$DATA_DIR/server.log
WAYFINDER_PID=
cleanup() {
  if [ -n "$WAYFINDER_PID" ] && kill -0 "$WAYFINDER_PID" 2>/dev/null; then
    kill -TERM "$WAYFINDER_PID"
    wait "$WAYFINDER_PID"
  fi
  rm -rf -- "$DATA_DIR"
}
trap cleanup EXIT INT TERM
assert_listener_free() {
  if curl --silent --output /dev/null --max-time 1 "http://$LISTENER/"; then
    echo "Refusing to use occupied listener $LISTENER" >&2
    exit 1
  fi
}
wait_for_wayfinder() {
  attempt=0
  while [ "$attempt" -lt 30 ]; do
    if ! kill -0 "$WAYFINDER_PID" 2>/dev/null; then
      wait "$WAYFINDER_PID" || true
      cat "$LOG"
      exit 1
    fi
    # This line is emitted by this child only after its bind succeeds.
    if grep -Fq "wayfinder listening" "$LOG" && \
      curl --fail --silent --output /dev/null --max-time 1 \
        "$BASE/admin/ping?wt=json"; then
      return
    fi
    attempt=$((attempt + 1))
    sleep 1
  done
  echo "Wayfinder did not become ready within 30 seconds" >&2
  exit 1
}
```

## Start and ping

Start on loopback, not a public interface. The process owns `DATA_DIR`; keep it
for the restart below.

```sh
assert_listener_free
: >"$LOG"
RUST_LOG=info "$WAYFINDER" "$SCHEMA" "$DATA_DIR" "$LISTENER" >"$LOG" 2>&1 &
WAYFINDER_PID=$!
wait_for_wayfinder
```

**Request:** `GET`; **Content-Type:** none (no request body); **core:**
`getting-started`; **visibility:** loopback only; **expected status:** `200`
JSON.

```sh
curl --fail --silent --show-error --request GET \
  --write-out '\nHTTP %{http_code}\n' \
  "$BASE/admin/ping?wt=json"
```

## Index and commit the canonical corpus

`commit=true` makes this HTTP update visible and durable for the later restart.

**Request:** `POST`; **Content-Type:** `application/json`; **core:**
`getting-started`; **visibility:** loopback only; **expected status:** `200`
JSON.

```sh
curl --fail --silent --show-error --request POST \
  --header 'Content-Type: application/json' \
  --data-binary "@$CORPUS" \
  --write-out '\nHTTP %{http_code}\n' \
  "$BASE/update?commit=true&wt=json"
```

## Query, filter, choose fields, sort, page, and facet

This query searches for `trail`, filters to `guides`, returns only three stored
fields, sorts by the fast numeric `rank`, then takes one result starting at the
second result. It also requests the fast `category` field's classic facet.
`--data-urlencode` keeps query syntax and commas correctly encoded.

**Request:** `GET`; **Content-Type:** none (no request body); **core:**
`getting-started`; **visibility:** loopback only; **expected status:** `200`
JSON.

```sh
curl --fail --silent --show-error --get \
  --data-urlencode 'q=trail' \
  --data-urlencode 'fq=category:guides' \
  --data-urlencode 'fl=id,title,rank' \
  --data-urlencode 'sort=rank asc' \
  --data-urlencode 'start=1' \
  --data-urlencode 'rows=1' \
  --data-urlencode 'facet=true' \
  --data-urlencode 'facet.field=category' \
  --data-urlencode 'wt=json' \
  --write-out '\nHTTP %{http_code}\n' \
  "$BASE/select"
```

The response has one `filters` document in `response.docs`; `response.numFound`
is `2`, and `facet_counts` includes the `guides` category.

## Inspect the operator UI

These three GET pages are read-only and are Wayfinder-owned rather than Solr
routes. The separate `/ui/synonyms` page also has a POST action that replaces
durable query synonyms; see the [UI route inventory](../reference/wire-routes.md#wayfinder-ui-routes).

**Request:** `GET`; **Content-Type:** none (no request body); **core:**
`getting-started` (this process's core); **visibility:** loopback only;
**expected status:** `200` HTML.

```sh
curl --fail --silent --show-error --request GET \
  --write-out '\nHTTP %{http_code}\n' \
  "http://$LISTENER/ui"
```

**Request:** `GET`; **Content-Type:** none (no request body); **core:**
`getting-started` (this process's core); **visibility:** loopback only;
**expected status:** `200` HTML.

```sh
curl --fail --silent --show-error --get \
  --data-urlencode 'q=trail' \
  --data-urlencode 'rows=1' \
  --data-urlencode 'wt=json' \
  --write-out '\nHTTP %{http_code}\n' \
  "http://$LISTENER/ui/query"
```

**Request:** `GET`; **Content-Type:** none (no request body); **core:**
`getting-started` (this process's core); **visibility:** loopback only;
**expected status:** `200` HTML.

```sh
curl --fail --silent --show-error --request GET \
  --write-out '\nHTTP %{http_code}\n' \
  "http://$LISTENER/ui/stats"
```

## Stop gracefully and prove a restart uses the same data

Send `SIGTERM`, not `SIGKILL`; Wayfinder drains requests and flushes pending
writes before exit. Restart with the same schema and data directory, then query
the committed documents.

```sh
kill -TERM "$WAYFINDER_PID"
wait "$WAYFINDER_PID"
WAYFINDER_PID=

assert_listener_free
: >"$LOG"
RUST_LOG=info "$WAYFINDER" "$SCHEMA" "$DATA_DIR" "$LISTENER" >"$LOG" 2>&1 &
WAYFINDER_PID=$!
wait_for_wayfinder
```

**Request:** `GET`; **Content-Type:** none (no request body); **core:**
`getting-started`; **visibility:** loopback only; **expected status:** `200`
JSON.

```sh
curl --fail --silent --show-error --get \
  --data-urlencode 'q=*:*' \
  --data-urlencode 'rows=0' \
  --data-urlencode 'wt=json' \
  --write-out '\nHTTP %{http_code}\n' \
  "$BASE/select"

kill -TERM "$WAYFINDER_PID"
wait "$WAYFINDER_PID"
WAYFINDER_PID=
```

The final response has `response.numFound: 3`, proving the committed corpus
survives a graceful stop and restart.

## Security and backup minimum

Keep this listener on loopback or a trusted private network. Wayfinder is HTTP
only: terminate TLS before any remote client, and do not send Basic credentials
without that protection. Back up the matching schema, server configuration, and
binary version with the data directory. Do not copy a writable live data
directory with `cp`, `rsync`, or `tar`. `wayfinder snapshot` is only an online
index/schema/analyzer snapshot and omits durable `synonyms.txt`; a complete
backup requires a graceful stop and whole-directory copy. See
[`docs/DEPLOYMENT.md`](../../docs/DEPLOYMENT.md) before an operational deployment.
