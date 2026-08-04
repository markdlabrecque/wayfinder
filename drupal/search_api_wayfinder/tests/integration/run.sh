#!/usr/bin/env bash
# Integration verification for issue #80 (part of #57 M1 follow-up):
# installs a real Drupal site with the search_api_wayfinder module under
# test (backend plugin id "wayfinder", from M1/#75 -- no search_api_solr,
# no Solarium, no connector plugin), points an authenticated Search API server
# directly at a real Wayfinder instance built from this repo's
# `presets/search-api.toml`, and drives a real index + fulltext search round
# trip through WayfinderBackend::search().
#
# Gated behind WAYFINDER_INTEGRATION=1, the same way tests/differential.rs
# gates its live-Solr mode behind WAYFINDER_DIFF_SOLR=1: this harness is
# NOT part of default `cargo test` / `vendor/bin/phpunit` CI. Run manually:
#
#   WAYFINDER_INTEGRATION=1 bash drupal/search_api_wayfinder/tests/integration/run.sh
#
# Requires Docker with network access. Deliberately not a default job in
# .github/workflows/ci.yml (M5/#79 decided it stays manual, since Docker +
# network breaks the hermetic-gate contract the PHP unit job upholds); see
# that file for the workflow_dispatch job that runs this script on demand.
#
# Own isolated containers/ports (wf80-*, 18990/9080 -- see docker-compose.yml
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
# dropped. As of M5/#79 WayfinderBackend::viewSettings() does call
# {core}/admin/system for the version handshake (issue #59's endpoint), but
# it is covered hermetically by WayfinderBackendTest against the captured
# solr-ref/responses/admin_system.json fixture, so this harness still has no
# reason to probe it live.

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

chmod -R u+w drupal-site 2>/dev/null || true
rm -rf drupal-site
mkdir -p drupal-site

echo "--- building wayfinder image + starting wayfinder ---"
docker compose up -d --build wayfinder

echo -n "waiting for wayfinder ping"
wayfinder_ready=0
for _ in $(seq 60); do
  if curl -sf "http://localhost:18990/wayfinder/content/admin/ping?wt=json" >/dev/null 2>&1; then
    echo " ok"; wayfinder_ready=1; break
  fi
  echo -n "."; sleep 1
done

if [ "$wayfinder_ready" != "1" ]; then
  echo "FAIL: wayfinder never became ready (ping did not succeed after 60 attempts)"
  exit 1
fi

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

echo "--- confirming search_api_solr / solarium are NOT dependencies (acceptance item) ---"
docker exec wf80-drupal bash -lc "
  cd /opt/drupal
  if composer show drupal/search_api_solr >/dev/null 2>&1; then
    echo 'FAIL: drupal/search_api_solr present in dependency tree'
    exit 1
  fi
  if composer show solarium/solarium >/dev/null 2>&1; then
    echo 'FAIL: solarium/solarium present in dependency tree'
    exit 1
  fi
  echo 'confirmed: no drupal/search_api_solr or solarium/solarium in dependency tree'
"

echo "--- site install (sqlite) ---"
docker exec wf80-drupal bash -lc "
  cd /opt/drupal
  vendor/bin/drush site:install standard \
    --db-url=sqlite://sites/default/files/.ht.sqlite \
    --site-name='Wayfinder IT' --account-name=admin --account-pass=admin -y
  vendor/bin/drush en search_api search_api_wayfinder node file -y
"

echo "--- module install / backend plugin discovery check ---"
docker exec wf80-drupal bash -lc "
  cd /opt/drupal
  vendor/bin/drush pml --filter=search_api_wayfinder --format=json
  vendor/bin/drush php:eval \"print_r(array_keys(\\\\Drupal::service('plugin.manager.search_api.backend')->getDefinitions()));\"
"

echo "--- server, index, content ---"
docker cp create_content.php wf80-drupal:/opt/drupal/create_content.php
docker cp setup_server_index.php wf80-drupal:/opt/drupal/setup_server_index.php
docker cp run_queries.php wf80-drupal:/opt/drupal/run_queries.php
docker exec wf80-drupal bash -lc "
  cd /opt/drupal
  # Setup before content: setup_server_index.php creates the field_attachments
  # file field (and the server/index). The attachment node created by
  # create_content.php references that field, so the field must exist first --
  # otherwise the file reference is silently dropped on save and the #262
  # extraction slice has nothing to extract.
  vendor/bin/drush php:script setup_server_index.php
  vendor/bin/drush php:script create_content.php
  vendor/bin/drush search-api:index wf80_index || vendor/bin/drush sapi-i wf80_index
"

# WayfinderBackend sends commitWithin=1000ms (setup_server_index.php), an
# async *scheduled* hard commit, not immediate -- so the just-indexed fields
# are not yet visible to /select without this. Force a synchronous commit
# straight to the wayfinder container so the round trip isn't racing it.
curl -sf --user operator:secret "http://localhost:18990/wayfinder/content/update?commit=true" -H 'Content-Type: application/json' -d '{}' >/dev/null

# Assert documents actually landed before handing off to run_queries.php,
# so the "indexing succeeded" claim above is backed by real evidence, not
# just this comment.
num_found="$(curl -sf --user operator:secret --get "http://localhost:18990/wayfinder/content/select" \
  --data-urlencode 'q=*:*' \
  --data-urlencode 'fq=index_id:"wf80_index"' \
  --data-urlencode 'rows=0' \
  | jq -r '.response.numFound // 0')"
if ! [ "$num_found" -ge 1 ] 2>/dev/null; then
  echo "FAIL: expected indexed documents for index_id=wf80_index, found $num_found"
  exit 1
fi
echo "confirmed: $num_found document(s) indexed for wf80_index"

echo "--- real index+search round trip ---"
docker exec wf80-drupal bash -lc "
  cd /opt/drupal
  vendor/bin/drush php:script run_queries.php
"

echo "--- done ---"
