# Wayfinder vs Solr 9 -- benchmark results (issue #13)

Measured against PRD §8 targets. 50k-doc corpus, generated deterministically by `bench/src/corpus.rs` (seed 42). See `bench/run.sh` for the exact measurement procedure and `bench/README.md` for how to reproduce, including the 2M-doc run.

| Metric | Solr baseline | Wayfinder target | Solr measured | Wayfinder measured |
|---|---|---|---|---|
| Resident memory, idle | ~1 GB | < 50 MB | 744.6 MB | 121.8 MB |
| Resident memory, 2M docs under query load | 2-4 GB | < 500 MB | 752.5 MB | 124.4 MB |
| Cold start to first query served | 10-30 s | < 1 s | 1.54 s | 0.27 s |
| p95 query latency (facet+filter+highlight, 50k docs) | baseline | <= baseline | 8.21 ms | 2.16 ms |
| Container image size | ~500 MB | < 30 MB | 659.4 MB | 11.8 MB |
| Index size on disk | baseline | <= 1.2x baseline | 22.0 MB | 8.4 MB |

## Notes

- **"Resident memory, 2M docs under query load" is measured from the 50k run**, not a real 2M-doc run: the 2M corpus is not automated (see `bench/README.md`), so this row's measured columns are the 50k under-load numbers, not the PRD's 2M scenario. Real 2M numbers require running `bench/run.sh 42 2000000` and are not yet captured.
- Measured on a local Docker Desktop/OrbStack host, not dedicated hardware; absolute numbers (especially Solr cold start, which benefits from a warm image cache and may not reflect a cold pull) will vary by machine. Reproduce locally with `bench/run.sh` for numbers specific to your environment.
- Wayfinder's resident memory is host-process RSS (`ps -o rss=`); Solr's is `docker stats`' cgroup memory accounting. Both reflect real resident memory, but via different measurement paths, since Wayfinder runs as a native binary and Solr runs in a container in this harness.
