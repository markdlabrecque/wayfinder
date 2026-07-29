#!/usr/bin/env bash
# Rerun script for the search_api_solr contract capture (issue #55).
#
# Stands up a stock, unmodified Drupal + Search API + search_api_solr site
# against a real solr:9 in Docker, with an mitmproxy reverse proxy sitting
# between the module and Solr so every request/response pair the module
# actually sends can be frozen verbatim. This is a DISCOVERED contract: no
# fixture content here is hand-written, it is all captured traffic.
#
# Deliberately separate container names/ports from solr-ref/capture.sh's
# wayfinder-solr-ref stack (see CLAUDE.md hot-files table) so this cannot
# collide with other branches' captures. It also never touches
# solr-ref/responses/ or solr-ref/manifest.tsv.
#
# Version pins (recorded again in docs/reports/2026-07-29-search-api-capture.md):
#   Drupal core:        11.3.2
#   drupal/search_api:   1.41.0
#   drupal/search_api_solr: 4.4.0
#   drush:               13.7.6
#   Solr:                9 (image resolved to 9.10.1 at capture time)
#
# Requires Docker and network access (packagist.org, ftp.drupal.org). Takes
# several minutes (composer install of a full Drupal site). Tears itself
# down at the end regardless of success/failure.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BUILD="$HERE/build"
TRACE="$HERE/trace"
CONFIGSET="$HERE/configset"

cleanup() {
  echo "--- tearing down capture-only containers ---"
  (cd "$BUILD" && docker compose down -v) || true
}
trap cleanup EXIT

mkdir -p "$BUILD/drupal-site" "$BUILD/mitm-captures"
rm -f "$BUILD/mitm-captures"/*.json "$BUILD/mitm-captures/_counter"

cd "$BUILD"

echo "--- solr + mitm up ---"
docker compose up -d solr mitm

echo -n "waiting for solr"
for _ in $(seq 60); do
  if curl -sf "http://localhost:8996/solr/search_api_capture/admin/ping?wt=json" >/dev/null 2>&1; then
    echo " ok"; break
  fi
  echo -n "."; sleep 1
done

echo "--- drupal container up ---"
docker compose up -d drupal
sleep 3

echo "--- composer project (pinned versions) ---"
docker exec wf55-drupal bash -lc "
  set -euo pipefail
  cd /opt/drupal
  if [ ! -f composer.json ]; then
    composer create-project drupal/recommended-project:11.3.2 tmp_build --no-interaction
    shopt -s dotglob
    mv tmp_build/* .
    rmdir tmp_build
  fi
  composer require drush/drush:13.7.6 drupal/search_api:1.41.0 drupal/search_api_solr:4.4.0 --no-interaction
"

echo "--- site install (sqlite) ---"
docker exec wf55-drupal bash -lc "
  cd /opt/drupal
  vendor/bin/drush site:install standard \
    --db-url=sqlite://sites/default/files/.ht.sqlite \
    --site-name='SearchAPI Capture' --account-name=admin --account-pass=admin -y
  vendor/bin/drush en search_api search_api_solr node -y
"

echo "--- fields, server, index, corpus ---"
for f in setup_fields.php setup_server_index.php create_content.php; do
  cp "$BUILD/$f" "$BUILD/drupal-site/$f"
  docker exec wf55-drupal bash -lc "cd /opt/drupal && vendor/bin/drush php:script $f"
done

echo "--- config-set export, deploy to Solr ---"
docker exec wf55-drupal bash -lc "cd /opt/drupal && vendor/bin/drush search-api-solr:get-server-config capture_server /tmp/configset"
docker exec wf55-drupal php -r '
  $zip = new ZipArchive();
  $zip->open("/tmp/configset");
  $zip->extractTo("/tmp/configset_dir");
  $zip->close();
'
rm -rf "$CONFIGSET"; mkdir -p "$CONFIGSET"
docker cp wf55-drupal:/tmp/configset_dir/. "$CONFIGSET/"

docker cp "$CONFIGSET/." wf55-solr:/var/solr/data/search_api_capture/conf/
docker exec wf55-solr bash -lc "rm -f /var/solr/data/search_api_capture/conf/managed-schema.xml"
curl -s "http://localhost:8996/solr/admin/cores?action=RELOAD&core=search_api_capture&wt=json" >/dev/null

echo "--- fix connector path (jump-start default is wrong for a reverse proxy at proxy root) ---"
docker exec wf55-drupal bash -lc "cd /opt/drupal && vendor/bin/drush php:eval '
  \$server = \Drupal\search_api\Entity\Server::load(\"capture_server\");
  \$config = \$server->getBackendConfig();
  \$config[\"connector_config\"][\"path\"] = \"/\";
  \$server->setBackendConfig(\$config);
  \$server->save();
'"

echo "--- reset trace counter, then drive real indexing + search traffic ---"
: > "$BUILD/mitm-captures/_counter"
docker exec wf55-drupal bash -lc "cd /opt/drupal && vendor/bin/drush search-api:rebuild-tracker capture_index && vendor/bin/drush search-api:index capture_index"

cp "$BUILD/run_queries.php" "$BUILD/drupal-site/run_queries.php"
docker exec wf55-drupal bash -lc "cd /opt/drupal && vendor/bin/drush php:script run_queries.php"

echo "--- admin/handshake calls ---"
docker exec wf55-drupal bash -lc "cd /opt/drupal && vendor/bin/drush php:eval '
  \$connector = \Drupal\search_api\Entity\Server::load(\"capture_server\")->getBackend()->getSolrConnector();
  \$connector->getServerInfo(TRUE);
  \$connector->getLuke();
  \$connector->getStatsSummary();
  \$connector->getSchemaVersion(TRUE);
  \$terms = \$connector->getTermsQuery();
  \$terms->setFields([\"tm_X3b_en_title\"]);
  \$connector->execute(\$terms);
'"

echo "--- freeze trace + manifest ---"
rm -rf "$TRACE"; mkdir -p "$TRACE"
cp "$BUILD/mitm-captures"/*.json "$TRACE/"

python3 - "$TRACE" "$HERE/manifest.tsv" <<'PYEOF'
import json, glob, os, sys, urllib.parse

trace_dir, manifest_path = sys.argv[1], sys.argv[2]
rows = []
for f in sorted(glob.glob(os.path.join(trace_dir, "*.json"))):
    d = json.load(open(f))
    req, resp = d["request"], d["response"]
    path = req["path"]
    endpoint = path.split("?")[0]
    q = ""
    if "?" in path:
        qs = urllib.parse.parse_qs(path.split("?", 1)[1], keep_blank_values=True)
        q = qs.get("q", [""])[0]
    rows.append((os.path.basename(f), d["seq"], req["method"], endpoint, resp["status_code"], q[:80]))

with open(manifest_path, "w") as out:
    out.write("file\tseq\tmethod\tendpoint\tstatus\tq_prefix\n")
    for r in rows:
        out.write("\t".join(str(x) for x in r) + "\n")
PYEOF

echo "--- done: $(ls "$TRACE" | wc -l) trace files, config-set in $CONFIGSET ---"
