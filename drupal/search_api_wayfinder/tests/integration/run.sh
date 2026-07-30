#!/usr/bin/env bash
# Integration verification for issue #80 (part of #57 M1 follow-up):
# installs a real Drupal site with the search_api_wayfinder module under
# test (backend plugin id "wayfinder", from M1/#75 -- no search_api_solr,
# no Solarium, no connector plugin), points a Search API server directly at
# a real Wayfinder instance built from this repo's `presets/search-api.toml`,
# and drives a real index + fulltext search round trip through
# WayfinderBackend::search().
#
# Gated behind WAYFINDER_INTEGRATION=1, the same way tests/differential.rs
# gates its live-Solr mode behind WAYFINDER_DIFF_SOLR=1: this harness is
# NOT part of default `cargo test` / `vendor/bin/phpunit` CI. Run manually:
#
#   WAYFINDER_INTEGRATION=1 bash drupal/search_api_wayfinder/tests/integration/run.sh
#
# Requires Docker with network access. Not hooked into
# .github/workflows/ci.yml as a default job (M5/#79 owns full CI polish);
# see that file for an optional workflow_dispatch job that runs this script
# on demand.
#
# Own isolated containers/ports (wf80-*, 18983/9080 -- see docker-compose.yml
# comment for the full collision-avoidance rationale). Never touches
# solr-ref/search-api/ (read-only reference) or its capture.sh/manifest.tsv.
# Tears itself down at the end regardless of success/failure.
#
# Adapted from the old worktree's run.sh
# (/Users/mark/Projects/wayfinder-57-search-api-wayfinder/drupal/search_api_wayfinder/tests/integration/run.sh):
# same docker-compose/site-install/content/query orchestration shape. What
# changed: no `search_api_solr` dependency, no connector plugin discovery
# check, no site_hash/connector_config wiring -- the module under test now
# registers its own backend plugin directly (`drush pml`/`drush php:eval`
# check below asserts the "wayfinder" *backend* is discoverable, not a
# connector), and the admin/info/system probe from the old script is
# dropped (issue #59 merged the version-handshake endpoint; WayfinderBackend
# does not call it as of M1, so there is nothing to probe here yet).

if [ "${WAYFINDER_INTEGRATION:-0}" != "1" ]; then
  echo "skipping search_api_wayfinder integration harness (set WAYFINDER_INTEGRATION=1 to run)"
  exit 0
fi

set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$HERE"

cleanup() {
  echo "--- tearing down wf80-* containers ---"
  docker compose down -v || true
}
trap cleanup EXIT

rm -rf drupal-site
mkdir -p drupal-site

echo "--- building wayfinder image + starting wayfinder ---"
docker compose up -d --build wayfinder

echo -n "waiting for wayfinder ping"
for _ in $(seq 60); do
  if curl -sf "http://localhost:18983/solr/content/admin/ping?wt=json" >/dev/null 2>&1; then
    echo " ok"; break
  fi
  echo -n "."; sleep 1
done

echo "--- drupal container up ---"
docker compose up -d drupal
sleep 3

echo "--- composer project + module under test (path repo), no search_api_solr ---"
docker exec wf80-drupal bash -lc "
  set -euo pipefail
  cd /opt/drupal
  if [ ! -f composer.json ]; then
    composer create-project drupal/recommended-project:11.3.2 tmp_build --no-interaction
    shopt -s dotglob
    mv tmp_build/* .
    rmdir tmp_build
  fi
  php -r '\$c = json_decode(file_get_contents(\"composer.json\"), true); \$c[\"repositories\"][\"wf80_module\"] = [\"type\" => \"path\", \"url\" => \"/opt/module-src\", \"options\" => [\"versions\" => [\"wayfinder/search_api_wayfinder\" => \"dev-main\"]]]; file_put_contents(\"composer.json\", json_encode(\$c, JSON_PRETTY_PRINT | JSON_UNESCAPED_SLASHES));'
  composer config repositories.drupal composer https://packages.drupal.org/8
  composer require drush/drush:13.7.6 drupal/search_api:1.41.0 'wayfinder/search_api_wayfinder:dev-main' --no-interaction
"

echo "--- confirming search_api_solr is NOT a dependency (acceptance item) ---"
docker exec wf80-drupal bash -lc "
  cd /opt/drupal
  if composer why drupal/search_api_solr >/dev/null 2>&1; then
    echo 'FAIL: drupal/search_api_solr present in dependency tree'
    exit 1
  fi
  echo 'confirmed: no drupal/search_api_solr in dependency tree'
"

echo "--- site install (sqlite) ---"
docker exec wf80-drupal bash -lc "
  cd /opt/drupal
  vendor/bin/drush site:install standard \
    --db-url=sqlite://sites/default/files/.ht.sqlite \
    --site-name='Wayfinder IT' --account-name=admin --account-pass=admin -y
  vendor/bin/drush en search_api search_api_wayfinder node -y
"

echo "--- module install / backend plugin discovery check ---"
docker exec wf80-drupal bash -lc "
  cd /opt/drupal
  vendor/bin/drush pml --filter=search_api_wayfinder --format=json
  vendor/bin/drush php:eval \"print_r(array_keys(\\\\Drupal::service('plugin.manager.search_api.backend')->getDefinitions()));\"
"

echo "--- content, server, index ---"
docker cp create_content.php wf80-drupal:/opt/drupal/create_content.php
docker cp setup_server_index.php wf80-drupal:/opt/drupal/setup_server_index.php
docker cp run_queries.php wf80-drupal:/opt/drupal/run_queries.php
docker exec wf80-drupal bash -lc "
  cd /opt/drupal
  vendor/bin/drush php:script create_content.php
  vendor/bin/drush php:script setup_server_index.php
  vendor/bin/drush search-api:index wf80_index || vendor/bin/drush sapi-i wf80_index
"

# WayfinderBackend sends commitWithin=1000ms (setup_server_index.php), an
# async *scheduled* hard commit, not immediate -- so the just-indexed fields
# are not yet visible to /select without this. Force a synchronous commit
# straight to the wayfinder container so the round trip isn't racing it.
curl -sf "http://localhost:18983/solr/content/update?commit=true" -H 'Content-Type: application/json' -d '{}' >/dev/null

echo "--- real index+search round trip ---"
docker exec wf80-drupal bash -lc "
  cd /opt/drupal
  vendor/bin/drush php:script run_queries.php
"

echo "--- done ---"
