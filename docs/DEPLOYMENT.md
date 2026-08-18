# Deployment

Wayfinder runs one core per process: one schema, one data directory, and one listening address.
It serves HTTP only and should bind to loopback or a trusted private network. See
[CONFIGURATION.md](CONFIGURATION.md) before creating a production core.

## Install and run

Build from a tagged checkout or use the repository `Dockerfile`:

```sh
cargo build --locked --release
sudo install -o root -g root -m 0755 target/release/wayfinder /usr/local/bin/wayfinder
wayfinder <schema.toml> <data-dir> [bind-addr]
```

The bind address defaults to `127.0.0.1:8983`. `WAYFINDER_CONFIG` optionally names the server
configuration file; an unset variable or nonexistent path selects all defaults. That current
behavior means operators that intend to configure authentication must fail closed before startup:

```sh
test -r "$WAYFINDER_CONFIG" || { echo "WAYFINDER_CONFIG is missing or unreadable" >&2; exit 1; }
wayfinder <schema.toml> <data-dir> [bind-addr]
```

Run Wayfinder as a dedicated unprivileged user with exclusive read/write access to its data
directory. Protect the server configuration because it may contain Basic-auth credentials.

A conventional layout is:

```text
/usr/local/bin/wayfinder
/etc/wayfinder/content-schema.toml
/etc/wayfinder/content.toml
/var/lib/wayfinder/content/
```

Run another process on a different port and data directory for each additional core.

## systemd

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
ExecStartPre=/usr/bin/test -r /etc/wayfinder/content.toml
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

```sh
sudo systemctl daemon-reload
sudo systemctl enable --now wayfinder-content
curl --fail 'http://127.0.0.1:8983/wayfinder/content/admin/ping?wt=json'
journalctl -u wayfinder-content
```

SIGTERM stops new requests, drains in-flight work, and commits pending writes before exit. Reserve
SIGKILL for recovery from an unresponsive process.

## Docker Compose

The repository `Dockerfile` builds a static `scratch` image. Its `/tmp` is sticky and
world-writable (`01777`) so runtime UID/GID `65532` can stage multipart extraction. Publish its
port on host loopback only, or omit `ports` when clients share an internal container network.
Before `docker compose up`, fail closed on the host when authentication/configuration is intended:

```sh
test -r ./deploy/content.toml || { echo "missing Compose WAYFINDER_CONFIG" >&2; exit 1; }
```

```yaml
services:
  wayfinder-content:
    build: .
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

Create `./data/content` for UID/GID 65532 before first start.

## TLS and authentication

HTTP Basic credentials are plaintext-equivalent. Wayfinder deliberately leaves certificate
issuance, renewal, reload, and protocol policy to a reverse proxy such as Caddy, nginx, or Traefik.
Keep the proxy-to-Wayfinder hop on loopback, a trusted private network, or an encrypted tunnel.

```caddyfile
search.example.com {
    @public_health path /ui/ping /wayfinder/content/admin/ping
    respond @public_health 404
    reverse_proxy 127.0.0.1:8983
}
```

Caddy forwards `Authorization` automatically. Verify with:

```sh
curl -u operator 'https://search.example.com/wayfinder/content/select?q=*:*&rows=0'
```

## Backup

A complete backup contains the data directory, schema, server configuration, and preferably the
binary version or image digest.

### Online index snapshot (not a complete backup)

The destination must not exist and should be on the same filesystem as its parent:

```sh
stamp="$(date -u +%Y%m%dT%H%M%SZ)"
backup="/srv/backups/wayfinder/content-$stamp"
sudo install -d -m 0700 "$backup"
sudo -u wayfinder wayfinder snapshot /var/lib/wayfinder/content "$backup/data"
sudo cp -a /etc/wayfinder/content-schema.toml "$backup/schema.toml"
sudo cp -a /etc/wayfinder/content.toml "$backup/wayfinder.toml"
sudo sh -c "cd '$backup' && find . -type f ! -path ./SHA256SUMS -print0 | sort -z | xargs -0 sha256sum > SHA256SUMS"
```

`wayfinder snapshot` selects one committed Tantivy generation and atomically publishes a validated
copy of the **index, persisted schema, and analyzer contract only**. It is not a complete backup:
it intentionally omits `<data-dir>/synonyms.txt`, whose durable groups affect future query analysis.
Writes and queries continue during the snapshot; uncommitted updates belong to a later snapshot.

Do not use `cp`, `rsync`, or `tar` against a running writable data directory. Tantivy can replace
metadata and remove merged segments while those tools traverse it, producing an unrestorable copy.
A storage snapshot is suitable only when it is atomic across the whole directory and restore-tested.

### Complete backup: graceful stopped whole-directory copy

Use this as the safe complete method: stop Wayfinder gracefully, copy the **entire** data directory
(including `synonyms.txt`) and both configuration files, then restart and verify ping. Keep the
outage limited to the local copy; upload the completed backup after restart. Do not substitute
`wayfinder snapshot` for this complete backup because its deliberate omission of `synonyms.txt`
would lose durable query-synonym state.

Periodically restore and query backups. A successful copy command is not a restore test.

## Restore

Never merge a backup into an existing data directory.

1. Verify checksums before changing the live installation.
2. Stop Wayfinder gracefully.
3. Move the current data directory aside as a rollback copy.
4. Restore the backup into a fresh data directory and restore ownership.
5. Restore its matching schema and server configuration.
6. Start Wayfinder and verify ping, a representative query, and document counts.
7. Retain the rollback copy until application-level checks pass.

A mismatched schema may be rejected at startup. Treat that refusal as authoritative rather than
editing persisted index metadata.

## Reindex

Tantivy cannot alter an existing index schema. Adding, removing, or changing a static field—and
some analyzer-contract changes—requires a fresh data directory.

For Drupal Search API:

1. Stop Wayfinder and retain or back up the old data directory.
2. Create a new empty data directory with the Wayfinder user's ownership.
3. Install the new schema, normally `presets/search-api.toml`, and start Wayfinder.
4. Queue every Search API index sharing the core for reindexing.
5. Run cron or:

   ```sh
   drush search-api:reset-tracker <index-id>
   drush search-api:index <index-id>
   ```

6. Wait for or force the configured commit policy, then verify searches and per-`index_id` counts.
7. Retain the old data directory until validation completes.

## Upgrade checklist

1. Read release notes for schema or analyzer changes.
2. Record the current binary/image and create a tested backup.
3. Keep the old binary, schema, configuration, and data as one rollback set.
4. Start the new binary against existing data only when no reindex is required.
5. If startup requires a fresh index, follow **Reindex** rather than bypassing the check.
6. Verify health, indexing, representative queries, and counts before deleting the rollback set.
