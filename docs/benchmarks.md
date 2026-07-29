# Wayfinder vs Solr 9 -- benchmark results (issue #13)

Measured against PRD §8 targets. 50000-doc corpus, generated deterministically by `bench/src/corpus.rs` (seed 42). See `bench/run.sh` for the exact measurement procedure and `bench/README.md` for how to reproduce, including the 2M-doc run.

| Metric | Solr baseline | Wayfinder target | Solr measured | Wayfinder measured | Measurement path |
|---|---|---|---|---|---|
| Resident memory, idle | ~1 GB | < 50 MB | 744.6 MB | 121.8 MB | Solr: Docker container (`docker stats`). Wayfinder: native process (`ps -o rss=`). |
| Resident memory, 2M docs under query load | 2-4 GB | < 500 MB | not measured | not measured | Not measured: this run indexed 50000 docs, not 2M. |
| Cold start to first query served | 10-30 s | < 1 s | 1.54 s | 0.27 s | Solr: Docker container (`docker run` to first successful ping). Wayfinder: native process (binary launch to first successful ping). |
| p95 query latency (facet+filter+highlight, 50k docs) | baseline | <= baseline | 8.21 ms | 2.16 ms | Solr: HTTP to the Docker container's published port. Wayfinder: HTTP to the native process's bound port. |
| Container image size | ~500 MB | < 30 MB | 659.4 MB | 11.8 MB | Both: Docker image size (`docker inspect`), not a running-container measurement. |
| Index size on disk | baseline | <= 1.2x baseline | 22.0 MB | 8.4 MB | Solr: size inside the Docker container's data volume (`docker exec du`). Wayfinder: size of the native process's data directory on the host (`du`). |

## Notes

- **"Resident memory, 2M docs under query load" is only ever populated by a run with a 2M-doc corpus** (see the row's own "not measured" state above otherwise, and the "Measurement path" column for how each number was captured). The 2M corpus is not automated (see `bench/README.md`); real 2M numbers require running `bench/run.sh 42 2000000`.
- Measured on a local Docker Desktop/OrbStack host, not dedicated hardware; absolute numbers (especially Solr cold start, which benefits from a warm image cache and may not reflect a cold pull) will vary by machine. Reproduce locally with `bench/run.sh` for numbers specific to your environment.
- Every row's "Measurement path" column states, per engine, whether the number came from a Docker container or a native host process; Solr always runs in a Docker container in this harness and Wayfinder always runs as a native binary (except its image-size row, which measures the built image, not a running process), so the two engines' numbers are not directly comparable on overhead alone.
