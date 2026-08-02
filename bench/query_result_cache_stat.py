#!/usr/bin/env python3
"""Read one queryResultCache counter out of a Solr 9 admin/metrics response.

Usage: query_result_cache_stat.py <core> <stat>   (response body on stdin)

Consumes the body of

    GET <base>/admin/metrics?group=core&prefix=CACHE.searcher.queryResultCache&wt=json

on stdin and prints the named counter (e.g. `hits`, `lookups`) to stdout.

Why this endpoint and not `admin/mbeans?cat=CACHE&stats=true` (issue #251,
verified live against Solr 9): without `json.nl=map` Solr renders
`solr-mbeans` as a type *signature* -- HTTP 200 with a body that is not valid
JSON at all -- and with `json.nl=map` it becomes a dict, which is not the flat
[name, value, ...] array the old parse zipped. admin/metrics is plain nested
JSON with no NamedList flat/map ambiguity.

Note the endpoint is SERVER-level, not core-relative: every core shows up as
its own `solr.core.<core>` registry key inside the top-level `metrics` object,
so the core is selected here rather than assumed to be the only entry.

Every failure is loud (non-zero exit, message on stderr). A silently-zero
counter would turn run.sh's cold/warm cache assertions into a rubber stamp,
which is worse than not asserting at all.
"""

import json
import sys

BEAN = "CACHE.searcher.queryResultCache"


def main(argv):
    if len(argv) != 3:
        sys.exit("usage: query_result_cache_stat.py <core> <stat>  (metrics body on stdin)")
    core, stat = argv[1], argv[2]

    try:
        doc = json.load(sys.stdin)
    except ValueError as exc:
        sys.exit(f"query_result_cache_stat: admin/metrics response is not valid JSON: {exc}")

    metrics = doc.get("metrics")
    if not isinstance(metrics, dict):
        sys.exit("query_result_cache_stat: no `metrics` object in the admin/metrics response")

    registry = f"solr.core.{core}"
    if registry not in metrics:
        sys.exit(
            f"query_result_cache_stat: no `{registry}` registry in the admin/metrics response "
            f"(registries present: {sorted(metrics)})"
        )

    bean = metrics[registry].get(BEAN)
    if not isinstance(bean, dict):
        sys.exit(
            f"query_result_cache_stat: no `{BEAN}` bean under `{registry}` in the admin/metrics "
            f"response (did the request use prefix={BEAN}?)"
        )

    if stat not in bean:
        sys.exit(
            f"query_result_cache_stat: no `{stat}` counter in `{registry}`'s `{BEAN}` bean "
            f"(counters present: {sorted(bean)})"
        )

    print(int(bean[stat]))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
