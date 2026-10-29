# Tuning loop

Steps for an agent that makes Wayfinder faster by trial and error (issue
#456). Each try makes one change, measures it with `wf-bench`, and keeps it
only when it is a real win. The loop stops after `N` failed tries in a row, or
when the score reaches the target.

Run every command from the repo root. See [README.md](README.md) for what
`wf-bench` measures and how the score is built.

## Settings

The operator sets these before the loop starts:

| Setting | Default | Meaning |
|---|---|---|
| `N` | 5 | Failed tries in a row before the loop stops. |
| `TARGET` | 0.80 | Stop once the accepted score is at or below this. Lower is better. |
| `DOCS` | 10000 | `--docs` for every run. Keep it fixed for the whole loop. |

`DOCS` must not change mid-loop. The saved Solr results, the noise spread and
the accepted score are only comparable at the same doc count.

## The ideas log

`bench/ideas.tsv` records every idea tried, kept or dropped, across all loop
runs (issue #458), so a later run does not retry an idea that already failed.
It is append-only: add one row at the end of each try, and never edit or
delete an earlier row. The columns, tab-separated:

| Column | What it holds |
|---|---|
| `date` | The day of the try, `YYYY-MM-DD`. |
| `idea` | The change, in one line with no tabs. |
| `metric` | The metric the try targeted (step 2). |
| `delta` | The score change from `vs accepted <score>: <change>%`, e.g. `-4.7%`. `-` when a gate failed before the benchmark ran. |
| `outcome` | `kept`, or `dropped: <reason>` where the reason is `loss`, `noise`, `noise not measured`, or the failed gate (`fmt`, `clippy`, `test`, `build`, `correct`, `answers changed`). |
| `commit` | For `kept`, the short sha of the win's commit. For `dropped`, the short sha of `HEAD` the idea was tried on. |

Append a row with the values filled in, then commit it on its own:

```sh
printf '%s\t%s\t%s\t%s\t%s\t%s\n' "$(date +%F)" "<idea>" "<metric>" "<delta>" "<outcome>" "$(git rev-parse --short HEAD)" >> bench/ideas.tsv
git add bench/ideas.tsv
git commit -m "ideas: <idea> (<outcome>)"
```

The file is tracked, so an uncommitted row would be lost by the next revert.

## Set up once

1. Start on a clean tree: `git status` shows nothing to commit.
2. Fetch the corpus and build:

   ```sh
   bench/fetch-wiki.sh
   cargo build --release
   ```

3. Measure the noise of the current build, then accept it as the baseline:

   ```sh
   cargo run --release --manifest-path bench/Cargo.toml --bin wf-bench -- --docs $DOCS --noise 5
   cargo run --release --manifest-path bench/Cargo.toml --bin wf-bench -- --docs $DOCS --accept
   ```

   `--noise 5` writes `bench/out/noise.json`. `--accept` writes
   `bench/out/wf-baseline.json` and `bench/out/wf-answers.json`. Without both,
   no run can be called a `win`.

4. Set the failure count to 0.

## Each try

Below, "run the benchmark" means:

```sh
cargo run --release --manifest-path bench/Cargo.toml --bin wf-bench -- --docs $DOCS
```

### 1. Read the score

Run the benchmark on the accepted build, or reuse the latest JSON from the
last accepted run. Stdout is one line:

```
score 0.812 (wf/solr, <1 = wf wins) | vs accepted 0.815: -0.4% noise | p50 0.91 p99 0.77 cpu 0.85 mem 0.14 | correct 15/15 | PASS | bench/out/1761700000.json
```

The last field is the JSON detail, `bench/out/<unix-ts>.json`. Open it and
read `ratios`: one Wayfinder/Solr ratio per metric. Above 1.0 means Solr is
better at that metric.

### 2. Pick a target

Read `bench/ideas.tsv` first. Do not retry an idea already logged as
`dropped` unless you have a concrete reason it will work now, such as a kept
change since that it depended on.

Each metric's pull on the score is its weight in `bench/weights.tsv` times the
log of its ratio. Pick the metric with the largest `weight * ln(ratio)`. That
is where a speed-up moves the score most. Skip a metric you already failed on
in the last few tries unless you have a new idea for it.

### 3. Change the code

Make one small change aimed at that metric, in the Wayfinder crate (`src/`).
One idea per try, so the result says whether that idea worked.

Do not edit anything under `bench/`: not the driver, `weights.tsv`,
`queries.tsv`, nor `allowed-diffs.tsv`. Changing the measurement is not a
speed-up. The one exception is appending to `bench/ideas.tsv` in steps 5
and 6.

These files are protected: the loop is judged by them, so it may never
change them.

- `solr-ref/responses/`: Solr's saved answers
- `bench/allowed-diffs.tsv`
- `bench/queries.tsv`
- `bench/weights.tsv`
- `bench/src/score.rs`, `bench/src/check.rs`, `bench/src/protected.rs` and
  `bench/src/bin/wf-bench.rs`: the scoring and checking code

`--accept` saves a hash of each to `bench/out/wf-protected.json`. If any of
them differs, the benchmark refuses to score: it prints
`FAIL protected: <files>` and exits 1. When that happens, stop the loop and
report the files it named. Do not revert them to get past it, do not delete
`bench/out/wf-protected.json`, and never pass `--accept-protected`. Only a
person who has reviewed the change runs `--accept-protected`.

### 4. Run the gates

Run these in order. Stop at the first one that fails; the try has failed.
Revert and log it as in step 5.

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo build --release
```

Then run the benchmark. It runs `cargo test` again before measuring and
prints `FAIL tests` if it fails. The try fails if:

- the run exits non-zero, or the line says `FAIL` anywhere. `FAIL protected`
  is different: it does not count as a try. Stop the loop (step 3);
- the correctness check is not full: `correct 15/15` must have both numbers
  equal, and no `FAIL` line follows it. That check proves Wayfinder's answers
  still match Solr's;
- an `answers changed: <shapes>` line follows it. A speed-up must not change
  what Wayfinder answers.

If the run exits 2 because the machine is busy, wait and run it again. That
does not count as a try. Do not pass `--force`.

### 5. Keep or revert

Read the verdict after `vs accepted <score>: <change>%`:

- `win`: the score dropped by more than the noise spread. Keep it (step 6).
- `loss`: the score rose by more than the noise. Revert.
- `noise`: the change is inside the noise. Revert. A change that cannot be
  told apart from noise is not a win.
- `noise not measured`: set-up step 3 was skipped. Revert, and run
  `--noise 5` before trying again.

To revert a failed try, throw away every change since the last commit:

```sh
git reset --hard HEAD
git clean -fd
```

`bench/out/` is gitignored, so this keeps the results.

Then log the dropped idea in `bench/ideas.tsv` and commit the row (see
[The ideas log](#the-ideas-log)). Do this after the revert, never before:
`git reset --hard` would throw away the new row. Add 1 to the failure count
and go to step 7.

### 6. Commit a win

Commit the change, then accept it as the new baseline:

```sh
git add -A
git commit -m "perf(<scope>): <what changed>" -m "score <old> -> <new> (<change>%, win)" -m "<metric> <old ratio> -> <new ratio>; bench/out/<unix-ts>.json"
cargo run --release --manifest-path bench/Cargo.toml --bin wf-bench -- --docs $DOCS --accept
```

The commit message looks like this:

```
perf(search): reuse the term dictionary reader across segments

score 0.812 -> 0.774 (-4.7%, win)
p99 0.77 -> 0.70; bench/out/1761700123.json
```

- `<old>` is the accepted score from `vs accepted <old>`, and `<new>` is the
  run's score. Both to three decimals.
- The type is always `perf(<scope>):`, with the area of the code you changed
  as the scope.
- Commit messages are ASCII only: write `->`, never an arrow character.

Commit before running with `--accept`, so the accepted run records the
commit's git sha.

Then log the kept idea in `bench/ideas.tsv`, with the win's commit as the
`commit` column, and commit the row (see [The ideas log](#the-ideas-log)).
Set the failure count back to 0.

### 7. Decide whether to go on

Stop when any is true:

- The failure count reaches `N`: `N` failed tries in a row. A failed try is a
  failed gate, a `loss`, or `noise`.
- The accepted score is at or below `TARGET`.
- A run printed `FAIL protected`.

Otherwise go back to step 1.

## When the loop stops

Report:

- why it stopped: `N` failures in a row, the target was hit, or a protected
  file changed (name each file the run listed);
- the score at the start and the accepted score at the end;
- each kept commit, with its score change;
- the ideas that were tried and reverted, one line each.

Take the kept and dropped ideas from the rows this run appended to
`bench/ideas.tsv`.

Leave the tree clean. The last commit is the log row for the last try.
