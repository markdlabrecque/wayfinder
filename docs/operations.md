# Wayfinder operator runbook

Wayfinder runs one Solr-compatible core per process. Each process has one schema, one data
directory, and one listening address. Use this page to deploy, back up, restore, reindex, and
upgrade that process.

> **Do not expose Wayfinder directly to the public internet.** Optional HTTP Basic authentication
> protects the application routes, but Wayfinder does not terminate TLS. Basic credentials are
> plaintext-equivalent without TLS, and the two health routes remain public. Bind to loopback or a
> trusted private network; for remote access, follow the supported reverse-proxy model and Caddy
> example in [Deployment and TLS termination](deployment.md).

## Process and directory layout

Build and install the release binary from a tagged source checkout, or use the repository
`Dockerfile`:

```sh
cargo build --locked --release
sudo install -o root -g root -m 0755 target/release/wayfinder /usr/local/bin/wayfinder
```

The command line is:

```text
wayfinder <schema.toml> <data-dir> [bind-addr]
```

`bind-addr` defaults to `127.0.0.1:8983`. `WAYFINDER_CONFIG` optionally names the separate server
configuration described in [the README](../README.md#server-config-wayfindertoml). An unset
variable, or a path that does not exist, selects all defaults. Check the path before starting;
a misspelled path does not fail startup.

A conventional host layout for a core named `content` is:

```text
/usr/local/bin/wayfinder
/etc/wayfinder/content-schema.toml
/etc/wayfinder/content.toml
/var/lib/wayfinder/content/
```

Run Wayfinder as a dedicated unprivileged user. The user needs read access to the binary, schema,
and server config and exclusive read/write access to its data directory. If the server config
contains Basic credentials, make it readable only by that user. For example:

```sh
sudo useradd --system --home-dir /var/lib/wayfinder --shell /usr/sbin/nologin wayfinder
sudo install -d -o wayfinder -g wayfinder -m 0750 /var/lib/wayfinder/content
sudo install -d -o root -g wayfinder -m 0750 /etc/wayfinder
sudo install -o root -g wayfinder -m 0640 content-schema.toml /etc/wayfinder/content-schema.toml
sudo install -o root -g wayfinder -m 0640 content.toml /etc/wayfinder/content.toml
```

### One core per process

A Wayfinder core is the unit named by `[core].name` in the schema. Run another process with a
different port and data directory for another core:

| Core | Bind address | Data directory |
|---|---|---|
| `content` | `127.0.0.1:8983` | `/var/lib/wayfinder/content` |
| `products` | `127.0.0.1:8984` | `/var/lib/wayfinder/products` |

In Drupal Search API, a **server** configuration points to one core through its scheme, host,
port, base path (normally `/solr`), and core name. More than one Search API index may share that
server/core; the backend keeps them separate with `index_id`. Use separate Wayfinder processes
when the indexes need separate failure, resource, schema, or backup boundaries.

## systemd

Save this as `/etc/systemd/system/wayfinder-content.service` and adjust the paths and port for the
core:

```ini
[Unit]
Description=Wayfinder search core: content
After=network.target

[Service]
Type=simple
User=wayfinder
Group=wayfinder
Environment=WAYFINDER_CONFIG=/etc/wayfinder/content.toml
Environment=RUST_LOG=info
ExecStart=/usr/local/bin/wayfinder /etc/wayfinder/content-schema.toml /var/lib/wayfinder/content 127.0.0.1:8983
Restart=on-failure
RestartSec=2s
KillSignal=SIGTERM
TimeoutStopSec=60s
NoNewPrivileges=true
PrivateTmp=true
ProtectHome=true
ProtectSystem=strict
ReadWritePaths=/var/lib/wayfinder/content

[Install]
WantedBy=multi-user.target
```

Then enable it and check the core-specific ping route:

```sh
sudo systemctl daemon-reload
sudo systemctl enable --now wayfinder-content
sudo systemctl status wayfinder-content
curl --fail 'http://127.0.0.1:8983/solr/content/admin/ping?wt=json'
journalctl -u wayfinder-content
```

SIGTERM performs a graceful shutdown: Wayfinder stops accepting requests, drains in-flight work,
and commits pending `commitWithin`/autocommit writes before exiting. Do not replace it with
SIGKILL during normal operation.

## Docker Compose

The repository `Dockerfile` builds a static `scratch` image. This example publishes the port on
host loopback only, runs as an unprivileged numeric user, and persists the data in a bind mount:

```yaml
services:
  wayfinder-content:
    build:
      context: .
      dockerfile: Dockerfile
    user: "65532:65532"
    environment:
      WAYFINDER_CONFIG: /etc/wayfinder/wayfinder.toml
      RUST_LOG: info
    command:
      - /etc/wayfinder/schema.toml
      - /var/lib/wayfinder
      - 0.0.0.0:8983
    ports:
      - "127.0.0.1:8983:8983"
    volumes:
      - ./deploy/content-schema.toml:/etc/wayfinder/schema.toml:ro
      - ./deploy/content.toml:/etc/wayfinder/wayfinder.toml:ro
      - ./data/content:/var/lib/wayfinder
    restart: unless-stopped
    stop_grace_period: 60s
```

Create the host data directory for the same numeric user before the first start:

```sh
sudo install -d -o 65532 -g 65532 -m 0750 ./data/content
docker compose up -d --build
curl --fail 'http://127.0.0.1:8983/solr/content/admin/ping?wt=json'
```

Add another service with a different host port and data directory for another core. On a shared
Docker network, do not publish the port at all when only Drupal needs to reach it; configure the
Search API server host as the Compose service name.

## Backup

A backup is a snapshot of the entire data directory plus the schema and server config needed to
reopen it. Wayfinder provides an online snapshot command; the portable stop-copy-start procedure
below remains the fallback on platforms where atomic no-replace directory publication is not
available.

### Safe online snapshot

Create the destination on the same filesystem as its parent and ensure it does not already exist:

```sh
stamp="$(date -u +%Y%m%dT%H%M%SZ)"
backup="/srv/backups/wayfinder/content-$stamp"
sudo install -d -m 0700 "$backup"
sudo -u wayfinder wayfinder snapshot /var/lib/wayfinder/content "$backup/data"
sudo cp -a /etc/wayfinder/content-schema.toml "$backup/schema.toml"
sudo cp -a /etc/wayfinder/content.toml "$backup/wayfinder.toml"
sudo sh -c "cd '$backup' && find . -type f ! -path ./SHA256SUMS -print0 | sort -z | xargs -0 sha256sum > SHA256SUMS"
```

The command selects one committed Tantivy generation while briefly holding Tantivy's metadata
lock, opens every referenced immutable segment, releases the lock, and copies through those open
handles. Indexing, commits, merges, and queries continue during the copy. A private sibling staging
directory is validated and atomically published with no-replace semantics, so an existing
destination is never merged with or overwritten. Updates not committed when the generation is
selected belong to a later snapshot.

The snapshot already includes the index's persisted schema and analyzer marker. Keep the
operator-owned schema and server config alongside it as shown, because those are the explicit
restore inputs; protect the config because it may contain Basic credentials. Periodically restore
and query a snapshot rather than treating a successful command as sufficient evidence.

### A live recursive copy is not safe

The snapshot command is a protocol, not a wrapper around recursive copy. Tantivy atomically
replaces `meta.json` when committing, but `cp`, `rsync`, and `tar` do not create an atomic
filesystem snapshot. Commits and merges can remove files while they traverse the directory, or a
copy can capture new metadata without every segment that metadata references.

This was tested under continuous committed indexing load for issue [#233]: 20 ordinary recursive
copies were made and then opened as independent Wayfinder cores. Thirteen restored; seven failed
during traversal, and a controlled metadata-last copy referenced an uncopied `.term` file. Do not
use a raw recursive copy against a running writable data directory. A storage snapshot is suitable
only if it guarantees a point-in-time snapshot of the whole directory and its restore procedure is
independently validated.

[#233]: https://github.com/markdlabrecque/wayfinder/issues/233

### Portable stop-copy-start procedure

The following example creates a private backup directory. The maintenance window lasts only for
the graceful stop and local copy; copying or uploading that completed directory elsewhere can
happen after Wayfinder restarts.

```sh
set -eu
stamp="$(date -u +%Y%m%dT%H%M%SZ)"
backup="/srv/backups/wayfinder/content-$stamp"
stopped=0
restart_if_stopped() {
  if [ "$stopped" -eq 1 ]; then
    sudo systemctl start wayfinder-content
    stopped=0
  fi
}
trap restart_if_stopped EXIT

stopped=1
sudo systemctl stop wayfinder-content
if sudo systemctl is-active --quiet wayfinder-content; then
  echo 'Wayfinder did not stop' >&2
  exit 1
fi

sudo install -d -m 0700 "$backup"
sudo cp -a /var/lib/wayfinder/content "$backup/data"
sudo cp -a /etc/wayfinder/content-schema.toml "$backup/schema.toml"
sudo cp -a /etc/wayfinder/content.toml "$backup/wayfinder.toml"
sudo sh -c "cd '$backup' && find . -type f ! -path ./SHA256SUMS -print0 | sort -z | xargs -0 sha256sum > SHA256SUMS"

restart_if_stopped
trap - EXIT
curl --fail 'http://127.0.0.1:8983/solr/content/admin/ping?wt=json'
```

Protect the backup like the live server config because it may contain Basic credentials. Retain
the Wayfinder binary version alongside upgrade backups, or record its immutable image digest.
Periodically restore a backup into a temporary directory and query it; a successful copy command
alone is not a restore test.

For Compose, use `docker compose stop wayfinder-content`, copy `./data/content` and both config
files while the container is stopped, then use `docker compose start wayfinder-content`.

## Restore

Never merge a backup into an existing data directory. Restore to a fresh directory so stale
segments cannot mask an incomplete backup.

1. Stop the process gracefully.
2. Move the current data directory aside; do not delete it until verification finishes.
3. Copy the backup's `data` directory into a new data directory and restore its ownership.
4. Restore the matching schema and server config. A different schema may be rejected at startup.
5. Start Wayfinder and verify ping plus a representative query and document count.
6. Keep the old directory until application-level checks pass.

```sh
set -eu
backup=/srv/backups/wayfinder/content-YYYYMMDDTHHMMSSZ
stamp="$(date -u +%Y%m%dT%H%M%SZ)"
rollback="/var/lib/wayfinder/rollback-$stamp"
restore_in_progress=0
live_moved=0
recover_failed_restore() {
  status=$?
  trap - EXIT
  if [ "$status" -ne 0 ] && [ "$restore_in_progress" -eq 1 ]; then
    sudo systemctl stop wayfinder-content || true
    if [ "$live_moved" -eq 1 ]; then
      if [ -e /var/lib/wayfinder/content ]; then
        sudo mv /var/lib/wayfinder/content "$rollback/failed-data" || true
      fi
      sudo mv "$rollback/data" /var/lib/wayfinder/content || true
    fi
    sudo cp -a "$rollback/schema.toml" /etc/wayfinder/content-schema.toml || true
    sudo cp -a "$rollback/wayfinder.toml" /etc/wayfinder/content.toml || true
    sudo systemctl start wayfinder-content || true
  fi
  exit "$status"
}
trap recover_failed_restore EXIT

# Verify before changing the live installation.
sudo sh -c "cd '$backup' && sha256sum --check SHA256SUMS"
sudo install -d -m 0700 "$rollback"
sudo cp -a /etc/wayfinder/content-schema.toml "$rollback/schema.toml"
sudo cp -a /etc/wayfinder/content.toml "$rollback/wayfinder.toml"
restore_in_progress=1

sudo systemctl stop wayfinder-content
sudo mv /var/lib/wayfinder/content "$rollback/data"
live_moved=1
sudo cp -a "$backup/data" /var/lib/wayfinder/content
sudo chown -R wayfinder:wayfinder /var/lib/wayfinder/content
sudo cp -a "$backup/schema.toml" /etc/wayfinder/content-schema.toml
sudo cp -a "$backup/wayfinder.toml" /etc/wayfinder/content.toml
sudo systemctl start wayfinder-content
curl --fail 'http://127.0.0.1:8983/solr/content/admin/ping?wt=json'

# Set WAYFINDER_AUTH_USER to the configured username when [auth] is enabled.
# Supplying the username without a password makes curl prompt securely for it.
if [ -n "${WAYFINDER_AUTH_USER:-}" ]; then
  curl --fail --user "$WAYFINDER_AUTH_USER" --get \
    'http://127.0.0.1:8983/solr/content/select' \
    --data-urlencode 'q=*:*' --data-urlencode 'rows=0' --data-urlencode 'wt=json'
else
  curl --fail --get 'http://127.0.0.1:8983/solr/content/select' \
    --data-urlencode 'q=*:*' --data-urlencode 'rows=0' --data-urlencode 'wt=json'
fi
restore_in_progress=0
trap - EXIT
```

On failure after the stop, the trap attempts that rollback automatically and preserves a partial
restore as `$rollback/failed-data`. Check the service and the rollback directory before retrying;
commands marked `|| true` can still need manual repair after a filesystem or service-manager
failure. Do not delete the rollback directory until application-level validation passes.

## Reindex from Drupal

Reindex instead of restoring when the source of truth is Drupal, when no usable backup exists, or
when Wayfinder refuses startup because the schema/analyzer contract changed. Tantivy cannot alter
an existing index schema in place; adding, removing, or changing a static field also requires a
fresh data directory.

Plan a maintenance window unless routing to a separately prepared process is already part of your
deployment. For every Drupal Search API index that uses the affected server/core:

1. Stop Wayfinder gracefully and retain or back up the old data directory.
2. Create a new, empty data directory with the Wayfinder user's ownership. Do not clear or reuse
   the incompatible directory.
3. Install the new schema (normally `presets/search-api.toml`) and start Wayfinder against the new
   directory.
4. In Drupal, go to **Configuration → Search and metadata → Search API → _index_ → View** and use
   **Queue all items for reindexing**. Repeat for every Search API index sharing this server/core.
5. Run cron/indexing until each queue is empty, or run the equivalent Drush commands:

   ```sh
   drush search-api:reset-tracker <index-id>
   drush search-api:index <index-id>
   ```

6. Force or wait for the configured commit policy, then verify Drupal searches and compare the
   Wayfinder count for each `index_id`.
7. Keep the old data directory until the rebuilt index passes validation.

The backend maps multiple Search API indexes into one core, so rebuilding only one of them leaves
the others absent from a fresh data directory.

## Upgrade checklist

1. Read release notes for schema or analyzer-contract changes before replacing the binary.
2. Record the current binary version/image digest and make a stop-copy-start backup.
3. Keep the old binary, schema, config, and data directory as one rollback set.
4. Start the new binary against the existing data only when no reindex requirement is announced.
5. If startup says to reindex into a fresh data directory, do not edit the schema persisted
   inside the data directory or retry against the old directory. Follow **Reindex from Drupal**
   above.
6. Verify ping, Drupal indexing, representative queries, and document counts before deleting the
   rollback set.

In particular, indexes created with the old static built-in `text_en` analyzer contract are
rejected by analyzer-contract v2 (issue [#205]) because mixing old index-time terms with the new
Porter-compatible query analyzer would return inconsistent matches. Normal Search API dynamic
text indexes retain their prior analyzer contract, but the startup check remains authoritative:
if it refuses an index, rebuild it rather than bypassing the check.

[#205]: https://github.com/markdlabrecque/wayfinder/issues/205
