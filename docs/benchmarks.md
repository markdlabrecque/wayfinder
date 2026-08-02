# Wayfinder vs Solr 9 -- benchmark results (issue #13)

Measured against PRD §8 targets, on a corpus of 2000000 docs generated deterministically by `bench/src/corpus.rs` (seed 42). See `bench/run.sh` for the exact measurement procedure and `bench/README.md` for how to reproduce, including the 2M-doc run.

| Metric | Solr baseline | Wayfinder target | Solr measured | Wayfinder measured | Measurement path |
|---|---|---|---|---|---|
| Resident memory, startup idle | ~1 GB | < 50 MB | 737.8 MB | 9.2 MB | Solr: Docker container (`docker stats`). Wayfinder: native process (`ps -o rss=`). RSS includes allocator-resident memory plus mmap-backed index pages. |
| Resident memory, post-index before query load (2000000 docs) | No PRD baseline | No PRD target | 820.4 MB | 2149.3 MB | Solr: Docker container (`docker stats`). Wayfinder: native process (`ps -o rss=`). RSS includes allocator-resident memory plus mmap-backed index pages. |
| Resident memory, 2M docs under query load | 2-4 GB | < 500 MB | 858.7 MB | 2209.6 MB | Solr: Docker container (`docker stats`). Wayfinder: native process (`ps -o rss=`). RSS includes allocator-resident memory plus mmap-backed index pages. |
| Cold start to first query served | 10-30 s | < 1 s | 1.61 s | 0.42 s | Solr: Docker container (`docker run` to first successful ping). Wayfinder: native process (binary launch to first successful ping). |
| p95 query latency, warm cache (facet+filter+highlight, 2000000 docs) | baseline | <= baseline | 9.79 ms | 9.73 ms | Solr: HTTP to the Docker container's published port, served from Solr's queryResultCache. Wayfinder: HTTP to the native process's bound port; Wayfinder has no query result cache. |
| p95 query latency, cold cache (distinct queries, 2000000 docs) | baseline | <= baseline | 18.90 ms | 15.20 ms | Solr: HTTP to the Docker container's published port, after a core RELOAD flushed Solr's caches. Wayfinder: HTTP to the native process's bound port. Every query in this pass is distinct. |
| Container image size | ~500 MB | < 30 MB | 659.4 MB | 14.1 MB | Both: Docker image size (`docker inspect`), not a running-container measurement. |
| Index size on disk | baseline | <= 1.2x baseline | 389.5 MB | 330.8 MB | Solr: size inside the Docker container's data volume (`docker exec du`). Wayfinder: size of the native process's data directory on the host (`du`). |

## Notes

- This is a long manual local run outside CI; indexing is expected to dominate wall time.
- Wayfinder met the PRD's <50 MB startup-idle resident-memory target at 9.2 MB.
- Wayfinder missed the PRD's <500 MB query-load resident-memory target: 2149.3 MB was resident at the post-index sample, and RSS increased by 60.3 MB between that sample and the later maximum of 2209.6 MB.
- The harness does not distinguish allocator-resident memory from mmap-backed index pages.
- The two latency rows measure different cache conditions, and the PRD does not say which of the two p95 rows its `<= baseline` target refers to: the warm row compares a cached Solr against an uncached Wayfinder (Solr serves it from its queryResultCache; Wayfinder has no query result cache), while the cold row runs distinct queries against caches flushed by a core RELOAD. Neither row is reported here as meeting or missing a target, because which comparison the target means is an open product decision -- #251 tracks settling it.
- Measured on a local Docker Desktop/OrbStack host, not dedicated hardware; absolute numbers (especially Solr cold start, which benefits from a warm image cache and may not reflect a cold pull) will vary by machine. Reproduce locally with `bench/run.sh` for numbers specific to your environment.
- Every row's "Measurement path" column states, per engine, whether the number came from a Docker container or a native host process; Solr always runs in a Docker container in this harness and Wayfinder always runs as a native binary (except its image-size row, which measures the built image, not a running process), so the two engines' numbers are not directly comparable on overhead alone.
