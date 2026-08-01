# Wayfinder vs Solr 9 -- benchmark results (issue #13)

Measured against PRD §8 targets, on a corpus of 2000000 docs generated deterministically by `bench/src/corpus.rs` (seed 42). See `bench/run.sh` for the exact measurement procedure and `bench/README.md` for how to reproduce, including the 2M-doc run.

| Metric | Solr baseline | Wayfinder target | Solr measured | Wayfinder measured | Measurement path |
|---|---|---|---|---|---|
| Resident memory, startup idle | ~1 GB | < 50 MB | 736.7 MB | 9.0 MB | Solr: Docker container (`docker stats`). Wayfinder: native process (`ps -o rss=`). RSS includes allocator-resident memory plus mmap-backed index pages. |
| Resident memory, post-index before query load (2000000 docs) | No PRD baseline | No PRD target | 845.2 MB | 2153.8 MB | Solr: Docker container (`docker stats`). Wayfinder: native process (`ps -o rss=`). RSS includes allocator-resident memory plus mmap-backed index pages. |
| Resident memory, 2M docs under query load | 2-4 GB | < 500 MB | 915.4 MB | 2161.5 MB | Solr: Docker container (`docker stats`). Wayfinder: native process (`ps -o rss=`). RSS includes allocator-resident memory plus mmap-backed index pages. |
| Cold start to first query served | 10-30 s | < 1 s | 1.63 s | 0.29 s | Solr: Docker container (`docker run` to first successful ping). Wayfinder: native process (binary launch to first successful ping). |
| p95 query latency (facet+filter+highlight, 2000000 docs) | baseline | <= baseline | 9.84 ms | 13.92 ms | Solr: HTTP to the Docker container's published port. Wayfinder: HTTP to the native process's bound port. |
| Container image size | ~500 MB | < 30 MB | 659.4 MB | 13.9 MB | Both: Docker image size (`docker inspect`), not a running-container measurement. |
| Index size on disk | baseline | <= 1.2x baseline | 498.1 MB | 330.8 MB | Solr: size inside the Docker container's data volume (`docker exec du`). Wayfinder: size of the native process's data directory on the host (`du`). |

## Notes

- This is a long manual local run outside CI; indexing is expected to dominate wall time.
- Wayfinder met the PRD's <50 MB startup-idle resident-memory target at 9.0 MB.
- Wayfinder missed the PRD's <500 MB query-load resident-memory target: 2153.8 MB was resident at the post-index sample, and RSS increased by 7.7 MB between that sample and the 2161.5 MB maximum sampled during query load.
- The harness does not distinguish allocator-resident memory from mmap-backed index pages.
- Measured on a local Docker Desktop/OrbStack host, not dedicated hardware; absolute numbers (especially Solr cold start, which benefits from a warm image cache and may not reflect a cold pull) will vary by machine. Reproduce locally with `bench/run.sh` for numbers specific to your environment.
- Every row's "Measurement path" column states, per engine, whether the number came from a Docker container or a native host process; Solr always runs in a Docker container in this harness and Wayfinder always runs as a native binary (except its image-size row, which measures the built image, not a running process), so the two engines' numbers are not directly comparable on overhead alone.
