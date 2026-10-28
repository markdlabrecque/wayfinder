//! `wf-bench`: Wayfinder vs Solr speed, cost, and correctness (issue #440).
//!
//! Runs each engine in its own Docker container with identical CPU and memory
//! limits and host networking, one engine at a time, so neither competes with
//! the other for cores. Indexes the same docs into both, runs the same queries,
//! and compares the answers. CPU and memory come from each container's cgroup,
//! so both engines are measured the same way.
//!
//! The driver itself is pinned to cores the engines never get, and a run
//! refuses to start on a busy machine unless `--force` is given (issue #455).
//!
//! Stdout is one line, built for a hill-climb loop to read cheaply. Full detail
//! is written to `bench/out/<unix-ts>.json`, including the machine, the core
//! split, and both engine versions.
//!
//! Cold latency (issue #446) is measured `--cold` times per engine: stop the
//! container, evict its data dir from the page cache with `posix_fadvise`
//! (no root needed), start it again, and time the first request of each shape.
//!
//! Footprint (issue #451): `startup_ms` runs from container start to the first
//! 200 from `/select`, and `index_size_mb` sums the index dir on disk.
//!
//! Load (issue #447): searches per second with 1, 8 and 32 searches at once,
//! each level run for `LOAD_SECS` on the warm engine. Every user walks the same
//! varied URL sequence, which repeats no warm or cold seed.
//!
//! Before anything is measured, the repo's `cargo test` must pass. A build
//! that breaks the tests is never scored: the run prints `FAIL tests` and
//! exits non-zero.
//!
//! Usage: `wf-bench [--docs N] [--iters N] [--cold N] [--force]`
//! Requires: docker, a release build at `target/release/wayfinder`, and the
//! corpus from `bench/fetch-wiki.sh`.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use wayfinder_bench::check::check;
use wayfinder_bench::cold::{evict_page_cache, summarize};
use wayfinder_bench::footprint::{dir_size_bytes, time_to_first_search};
use wayfinder_bench::freshness;
use wayfinder_bench::load::{self, LEVELS, run_levels};
use wayfinder_bench::machine::{self, EnvInfo};
use wayfinder_bench::terms::{self, Pools};
use wayfinder_bench::wiki::{self, WikiDoc};

// ponytail: fixed core sets and memory cap, sized for the 16-core bench
// desktop. Engines share CPUSET (one runs at a time); the driver gets its own
// cores so its work never lands on an engine's.
const CPUSET: &str = "2-5";
const DRIVER_CPUSET: &str = "6-7";
// 1-minute load average above which the machine counts as busy.
const MAX_LOAD: f64 = 2.0;
const MEMORY: &str = "4g";
const SOLR_HEAP: &str = "2g";
const SOLR_PORT: u16 = 18983;
const WF_PORT: u16 = 18984;
const CORE: &str = "content";
// Per-request byte cap for indexing. Article sizes vary a lot, so batches
// are cut by size, not count. Under Wayfinder's 10 MB default body limit.
const BATCH_BYTES: usize = 4_000_000;
// Warm-up requests per query shape, so Solr's JIT has compiled the hot path
// before anything is timed. A fixed count, not a duration: both engines must
// see exactly the same request sequence.
const WARMUP: u64 = 300;
// Same-URL repeats per shape for the separate cache-hit metric.
const REPEAT: usize = 100;
// Seed offset for cold requests, past any warm seed (`WARMUP + 1 + iters`).
const COLD_SEED: u64 = 500_000;
// Seed offset for load requests, past any cold seed (`COLD_SEED + runs`).
const LOAD_SEED: u64 = 600_000;
// Rounds of every query shape in the load sequence each user walks.
const LOAD_ROUNDS: u64 = 200;
// Seconds each concurrency level runs for.
const LOAD_SECS: u64 = 10;
const TERM_SEED: u64 = 42;
// Add-then-find cycles per engine for the add-to-searchable time (#450).
const FRESHNESS_RUNS: usize = 20;
// Pinned so a re-run measures the same Solr and the same Wayfinder runtime.
const SOLR_IMAGE: &str =
    "solr@sha256:9ab3185bfc8e18e1a0c0d20083f783ec3ef2833b8add196bd0ebddc929525970";
const WF_RUNTIME_IMAGE: &str =
    "archlinux@sha256:e82623936fa1a69700b48e98b058df6eaab18b737c27473a7234a40fdecb9413";
const WF_SCHEMA: &str = r#"[core]
name = "content"
unique_key = "id"
default_field = "body"

[[fields]]
name = "id"
type = "string"
stored = true
required = true
fast = true

[[fields]]
name = "title"
type = "text_en"
stored = true

[[fields]]
name = "body"
type = "text_en"
stored = true

[[fields]]
name = "len"
type = "int"
stored = true
fast = true

[[fields]]
name = "len_bucket"
type = "string"
stored = true
fast = true

[[fields]]
name = "initial"
type = "string"
stored = true
fast = true
"#;

struct Query {
    name: String,
    mode: String,
    /// `(key, template)` pairs; templates are filled per request by `Pools::fill`.
    params: Vec<(String, String)>,
}

impl Query {
    fn url(&self, base: &str, pools: &Pools, i: u64) -> String {
        let params: Vec<String> = self
            .params
            .iter()
            .map(|(k, v)| format!("{k}={}", encode(&pools.fill(v, i))))
            .collect();
        format!("{base}/select?{}&wt=json", params.join("&"))
    }
}

fn encode(v: &str) -> String {
    v.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// Reads `name<TAB>mode<TAB>params` lines, skipping blanks and `#` comments.
fn tsv(path: &Path) -> Vec<Vec<String>> {
    std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
        .map(|l| l.split('\t').map(str::to_string).collect())
        .collect()
}

fn queries(path: &Path) -> Vec<Query> {
    tsv(path)
        .into_iter()
        .map(|cols| {
            let [name, mode, params] = <[String; 3]>::try_from(cols).expect("3 columns");
            let params = params
                .split('&')
                .map(|kv| {
                    let (k, v) = kv.split_once('=').expect("key=value");
                    (k.to_string(), v.to_string())
                })
                .collect();
            Query { name, mode, params }
        })
        .collect()
}

/// A running engine container. Removed on drop, so a panic mid-run never
/// leaves a container holding the port.
struct Engine {
    name: &'static str,
    container: String,
    cgroup: PathBuf,
    base: String,
    ping: String,
    /// Host dir holding the engine's index, bind-mounted and owned by the
    /// host user, so cold runs can evict it from the page cache without root.
    data: PathBuf,
    /// The index files alone, inside `data`, for the size on disk.
    index: PathBuf,
}

impl Drop for Engine {
    fn drop(&mut self) {
        let _ = Command::new("docker")
            .args(["rm", "-f", &self.container])
            .output();
    }
}

fn docker(args: &[&str]) -> String {
    let out = Command::new("docker")
        .args(args)
        .output()
        .expect("run docker");
    assert!(
        out.status.success(),
        "docker {}: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn start(
    name: &'static str,
    run_args: &[&str],
    base: String,
    ping: &str,
    data: PathBuf,
    index: PathBuf,
) -> (Engine, f64) {
    let container = format!("wf-bench-{name}");
    let _ = Command::new("docker")
        .args(["rm", "-f", &container])
        .output();
    let t0 = Instant::now();
    let mut args = vec![
        "run",
        "-d",
        "--name",
        &container,
        "--network",
        "host",
        "--cpuset-cpus",
        CPUSET,
        "--memory",
        MEMORY,
        "--memory-swap",
        MEMORY,
    ];
    args.extend_from_slice(run_args);
    let id = docker(&args);
    let engine = Engine {
        name,
        container,
        cgroup: PathBuf::from(format!("/sys/fs/cgroup/system.slice/docker-{id}.scope")),
        base,
        ping: ping.to_string(),
        data,
        index,
    };
    // Start-up ends at the first answered search, not the first ping: a
    // ping can pass before the core is ready to serve queries.
    let select = format!("{}/select?q=*:*&wt=json", engine.base);
    let startup_ms = time_to_first_search(&select, t0, Duration::from_secs(120))
        .unwrap_or_else(|e| panic!("{}: {e}", engine.name));
    (engine, startup_ms)
}

fn wait_ready(engine: &Engine, t0: Instant) {
    let url = format!("{}/{}", engine.base, engine.ping);
    while ureq::get(&url).call().is_err() {
        assert!(
            t0.elapsed() < Duration::from_secs(120),
            "{} never answered {url}",
            engine.name
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Stops the container, drops its index files from the page cache, and starts
/// it again: a fresh process with nothing cached. Stopping first matters, as
/// pages still mapped by the engine cannot be evicted. Solr's JVM comes back
/// cold too; that is part of a real cold start, so it stays in.
fn restart_cold(engine: &Engine) {
    docker(&["stop", &engine.container]);
    let files = evict_page_cache(&engine.data)
        .unwrap_or_else(|e| panic!("evict {}: {e}", engine.data.display()));
    assert!(files > 0, "{}: no index files to evict", engine.name);
    let t0 = Instant::now();
    docker(&["start", &engine.container]);
    wait_ready(engine, t0);
}

fn cgroup_u64(engine: &Engine, file: &str, key: Option<&str>) -> u64 {
    let text = std::fs::read_to_string(engine.cgroup.join(file)).expect("read cgroup file");
    match key {
        None => text.trim().parse().expect("cgroup value"),
        Some(k) => text
            .lines()
            .find_map(|l| l.strip_prefix(k)?.trim().parse().ok())
            .expect("cgroup key"),
    }
}

fn doc_json(d: &WikiDoc) -> Value {
    json!({
        "id": d.id, "title": d.title, "body": d.body,
        "len": d.len, "len_bucket": d.len_bucket, "initial": d.initial,
    })
}

fn post(url: &str, body: &Value) {
    let resp = ureq::post(url)
        .set("Content-Type", "application/json")
        .send_string(&body.to_string());
    if let Err(e) = resp {
        panic!("POST {url}: {e}");
    }
}

fn index(engine: &Engine, docs: &[WikiDoc]) -> f64 {
    let url = format!("{}/update?commit=false", engine.base);
    let t0 = Instant::now();
    let (mut batch, mut bytes) = (Vec::new(), 0);
    for d in docs {
        let doc = doc_json(d);
        let size = doc.to_string().len();
        if bytes + size > BATCH_BYTES && !batch.is_empty() {
            post(&url, &Value::Array(std::mem::take(&mut batch)));
            bytes = 0;
        }
        batch.push(doc);
        bytes += size;
    }
    if !batch.is_empty() {
        post(&url, &Value::Array(batch));
    }
    post(&format!("{}/update?commit=true", engine.base), &json!([]));
    docs.len() as f64 / t0.elapsed().as_secs_f64()
}

fn get(url: &str) -> (f64, Value) {
    let t0 = Instant::now();
    let body = ureq::get(url)
        .call()
        .unwrap_or_else(|e| panic!("GET {url}: {e}"))
        .into_string()
        .expect("response body");
    let ms = t0.elapsed().as_secs_f64() * 1000.0;
    (ms, serde_json::from_str(&body).expect("JSON response"))
}

fn pct(sorted: &[f64], p: f64) -> f64 {
    sorted[((sorted.len() - 1) as f64 * p).round() as usize]
}

/// Measures every query shape on one engine. Request `i` of shape `n` is
/// filled with seed `n * 1e6 + i`, so both engines get the identical sequence
/// and no timed request repeats an earlier one. Returns per-shape stats plus
/// every timed answer, for the correctness check.
fn measure(
    engine: &Engine,
    qs: &[Query],
    pools: &Pools,
    iters: usize,
) -> (Vec<Value>, Vec<Vec<Value>>) {
    let mut answers = Vec::new();
    let stats = qs
        .iter()
        .enumerate()
        .map(|(n, q)| {
            let url = |i: u64| q.url(&engine.base, pools, n as u64 * 1_000_000 + i);
            let (cold_ms, _) = get(&url(0));
            for i in 1..=WARMUP {
                get(&url(i));
            }
            let cpu0 = cgroup_u64(engine, "cpu.stat", Some("usage_usec"));
            let (mut lat, ans): (Vec<f64>, Vec<Value>) =
                (0..iters as u64).map(|i| get(&url(WARMUP + 1 + i))).unzip();
            let cpu_us = cgroup_u64(engine, "cpu.stat", Some("usage_usec")) - cpu0;
            let mut rep: Vec<f64> = (0..REPEAT).map(|_| get(&url(0)).0).collect();
            lat.sort_by(f64::total_cmp);
            rep.sort_by(f64::total_cmp);
            answers.push(ans);
            json!({
                "query": q.name,
                "cold_ms": cold_ms,
                "p50": pct(&lat, 0.50),
                "p95": pct(&lat, 0.95),
                "p99": pct(&lat, 0.99),
                "cpu_ms_per_1k": cpu_us as f64 / iters as f64,
                "repeat_p50": pct(&rep, 0.50),
            })
        })
        .collect();
    (stats, answers)
}

/// Cold latency per query shape: `runs` times, restart the engine with an
/// empty page cache, then time the first request of each shape. Seeds start
/// at `COLD_SEED` so no cold request repeats a warm one.
fn measure_cold(engine: &Engine, qs: &[Query], pools: &Pools, runs: usize) -> Value {
    let mut samples: Vec<(String, Vec<f64>)> =
        qs.iter().map(|q| (q.name.clone(), Vec::new())).collect();
    for r in 0..runs as u64 {
        restart_cold(engine);
        for (n, q) in qs.iter().enumerate() {
            let url = q.url(&engine.base, pools, n as u64 * 1_000_000 + COLD_SEED + r);
            samples[n].1.push(get(&url).0);
        }
    }
    summarize(&samples)
}

/// Searches per second at each of `LEVELS` users at once. The sequence takes
/// shapes in turn, round by round, so every user mixes all shapes.
fn measure_load(engine: &Engine, qs: &[Query], pools: &Pools) -> Value {
    let urls: Vec<String> = (0..LOAD_ROUNDS)
        .flat_map(|i| {
            qs.iter().enumerate().map(move |(n, q)| {
                q.url(&engine.base, pools, n as u64 * 1_000_000 + LOAD_SEED + i)
            })
        })
        .collect();
    load::to_json(&run_levels(&urls, &LEVELS, Duration::from_secs(LOAD_SECS)))
}

fn run_engine(
    engine: Engine,
    startup_ms: f64,
    commit_settings: &str,
    docs: &[WikiDoc],
    qs: &[Query],
    pools: &Pools,
    iters: usize,
    cold_runs: usize,
) -> (Value, Vec<Vec<Value>>) {
    let idle_rss = cgroup_u64(&engine, "memory.current", None);
    let docs_per_s = index(&engine, docs);
    let post_index_rss = cgroup_u64(&engine, "memory.current", None);
    let (queries, answers) = measure(&engine, qs, pools, iters);
    // After the queries, so the extra docs never show up in compared answers.
    let add_to_searchable = freshness::measure(&engine.base, FRESHNESS_RUNS, commit_settings);
    // Peak memory is read before the cold runs: a restart resets it.
    let mem_peak = cgroup_u64(&engine, "memory.peak", None);
    // After the peak read, so many users at once don't skew the memory score,
    // and before the cold runs, while the engine is still warm.
    let load = measure_load(&engine, qs, pools);
    let cold = measure_cold(&engine, qs, pools, cold_runs);
    // Read last, well after the final commit, so any merge the engine runs by
    // default after it has landed.
    let index_bytes = dir_size_bytes(&engine.index)
        .unwrap_or_else(|e| panic!("size {}: {e}", engine.index.display()));
    let stats = json!({
        "engine": engine.name,
        "startup_ms": startup_ms,
        "index_size_mb": index_bytes as f64 / 1048576.0,
        "index_docs_per_s": docs_per_s,
        "mem_idle_mb": idle_rss as f64 / 1048576.0,
        "mem_post_index_mb": post_index_rss as f64 / 1048576.0,
        "mem_peak_mb": mem_peak as f64 / 1048576.0,
        "queries": queries,
        "load": load,
        "cold_runs": cold_runs,
        "cold": cold,
        "add_to_searchable": add_to_searchable,
    });
    (stats, answers)
}

/// Returns the stats, the answers, and the Solr version that answered them.
fn run_solr(
    work: &Path,
    docs: &[WikiDoc],
    qs: &[Query],
    pools: &Pools,
    iters: usize,
    cold_runs: usize,
) -> (Value, Vec<Vec<Value>>, String) {
    let port_env = format!("SOLR_PORT={SOLR_PORT}");
    let heap_env = format!("SOLR_HEAP={SOLR_HEAP}");
    // Solr's home on the host, owned by the host user like Wayfinder's data
    // dir, so cold runs can evict it and cleanup can remove it.
    let data = work.join("solr");
    std::fs::create_dir_all(&data).expect("create Solr home");
    let solr_mount = format!("{}:/var/solr", data.display());
    // Lucene's index only, not the tlog, as `run.sh` measures it (#253).
    let index = data.join(format!("data/{CORE}/data/index"));
    let user = host_user();
    let (engine, startup_ms) = start(
        "solr",
        &[
            "-u",
            &user,
            "-v",
            &solr_mount,
            "-e",
            &port_env,
            "-e",
            &heap_env,
            SOLR_IMAGE,
            "solr-precreate",
            CORE,
        ],
        format!("http://127.0.0.1:{SOLR_PORT}/solr/{CORE}"),
        "admin/ping?wt=json",
        data,
        index,
    );
    post(
        &format!("{}/schema", engine.base),
        &json!({"add-field": [
            {"name": "title", "type": "text_en", "indexed": true, "stored": true},
            {"name": "body", "type": "text_en", "indexed": true, "stored": true},
            {"name": "len", "type": "pint", "indexed": true, "stored": true, "docValues": true},
            {"name": "len_bucket", "type": "string", "indexed": true, "stored": true,
             "docValues": true},
            {"name": "initial", "type": "string", "indexed": true, "stored": true,
             "docValues": true},
        ]}),
    );
    let info_url = format!("http://127.0.0.1:{SOLR_PORT}/solr/admin/info/system?wt=json");
    let info = ureq::get(&info_url)
        .call()
        .unwrap_or_else(|e| panic!("GET {info_url}: {e}"))
        .into_string()
        .expect("response body");
    let version = machine::parse_solr_version(&info).expect("solr-spec-version in system info");
    let commit = format!(
        "commitWithin={}ms on each add; stock _default configset autoCommit/autoSoftCommit",
        freshness::COMMIT_WITHIN_MS
    );
    let (stats, answers) =
        run_engine(engine, startup_ms, &commit, docs, qs, pools, iters, cold_runs);
    (stats, answers, version)
}

fn run_wayfinder(
    root: &Path,
    work: &Path,
    docs: &[WikiDoc],
    qs: &[Query],
    pools: &Pools,
    iters: usize,
    cold_runs: usize,
) -> (Value, Vec<Vec<Value>>) {
    let bin = root.join("target/release/wayfinder");
    assert!(bin.exists(), "build first: cargo build --release");
    std::fs::write(work.join("schema.toml"), WF_SCHEMA).expect("write schema");
    let bin_mount = format!("{}:/wayfinder:ro", bin.display());
    let work_mount = format!("{}:/work", work.display());
    let user = host_user();
    let bind = format!("127.0.0.1:{WF_PORT}");
    let (engine, startup_ms) = start(
        "wayfinder",
        &[
            "-u",
            &user,
            "-v",
            &bin_mount,
            "-v",
            &work_mount,
            WF_RUNTIME_IMAGE,
            "/wayfinder",
            "/work/schema.toml",
            "/work/data",
            &bind,
        ],
        format!("http://{bind}/wayfinder/{CORE}"),
        "admin/ping?wt=json",
        work.join("data"),
        work.join("data"),
    );
    let commit = format!(
        "commitWithin={}ms on each add; default config, no autocommit",
        freshness::COMMIT_WITHIN_MS
    );
    run_engine(engine, startup_ms, &commit, docs, qs, pools, iters, cold_runs)
}

/// `uid:gid` of the host user. Containers run as it so their data dirs stay
/// readable for page-cache eviction and removable afterwards.
fn host_user() -> String {
    format!("{}:{}", docker_id("-u"), docker_id("-g"))
}

fn docker_id(flag: &str) -> String {
    let out = Command::new("id").arg(flag).output().expect("id");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn git_sha(root: &Path) -> String {
    let out = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .current_dir(root)
        .output()
        .expect("git");
    assert!(out.status.success(), "git rev-parse failed");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn read(path: &str) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{path}: {e}"))
}

fn arg(args: &[String], flag: &str, default: usize) -> usize {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .map_or(default, |v| v.parse().expect("numeric flag"))
}

/// Runs the repo's `cargo test`. Its output goes to stderr, so stdout stays
/// the single line the hill-climb loop reads.
fn tests_pass(root: &Path) -> bool {
    Command::new("cargo")
        .arg("test")
        .current_dir(root)
        .stdout(Stdio::from(std::io::stderr()))
        .status()
        .is_ok_and(|s| s.success())
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let n_docs = arg(&args, "--docs", 10_000);
    let iters = arg(&args, "--iters", 200);
    let cold_runs = arg(&args, "--cold", 5);
    let force = args.iter().any(|a| a == "--force");
    let load = machine::parse_loadavg(&read("/proc/loadavg")).expect("1-min load average");
    let busy_warning = match machine::check_busy(load, MAX_LOAD, force) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("wf-bench: {e}");
            std::process::exit(2);
        }
    };
    if let Some(w) = &busy_warning {
        eprintln!("wf-bench: warning: {w}");
    }
    // Pin before anything else runs: docker CLI children inherit the mask,
    // the engine containers get CPUSET from dockerd.
    machine::validate_cores(CPUSET, DRIVER_CPUSET).expect("core split");
    let driver_cpus = machine::parse_cpu_list(DRIVER_CPUSET).expect("driver cores");
    machine::pin_to(&driver_cpus).expect("pin driver");

    let bench_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let root = bench_dir.parent().expect("repo root").to_path_buf();
    if !tests_pass(&root) {
        println!("FAIL tests");
        std::process::exit(1);
    }
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_secs();
    let work = bench_dir.join(format!("out/work-{ts}"));
    std::fs::create_dir_all(&work).expect("create work dir");

    let qs = queries(&bench_dir.join("queries.tsv"));
    let allowed: Vec<String> = tsv(&bench_dir.join("allowed-diffs.tsv"))
        .into_iter()
        .map(|cols| cols[0].clone())
        .collect();
    let docs = wiki::load(&bench_dir.join("data"), n_docs);
    let pools = terms::pools(&docs, TERM_SEED);
    let (solr, solr_answers, solr_version) =
        run_solr(&work, &docs, &qs, &pools, iters, cold_runs);
    let (wf, wf_answers) = run_wayfinder(&root, &work, &docs, &qs, &pools, iters, cold_runs);
    let _ = std::fs::remove_dir_all(&work);

    let mut failures = Vec::new();
    let mut waived = Vec::new();
    let mut p50 = Vec::new();
    let mut p99 = Vec::new();
    let mut cpu = Vec::new();
    let pairs = wf["queries"].as_array().unwrap().iter();
    for (n, ((qw, qsolr), q)) in pairs
        .zip(solr["queries"].as_array().unwrap())
        .zip(&qs)
        .enumerate()
    {
        let bad: Vec<(usize, String)> = wf_answers[n]
            .iter()
            .zip(&solr_answers[n])
            .enumerate()
            .filter_map(|(i, (w, s))| check(&q.mode, w, s).err().map(|e| (i, e)))
            .collect();
        if let Some((i, why)) = bad.first() {
            // Show the first failing request so it can be replayed by hand.
            let url = q.url("", &pools, n as u64 * 1_000_000 + WARMUP + 1 + *i as u64);
            let line = format!(
                "{}: {}/{iters} wrong, e.g. {why} [{url}]",
                q.name,
                bad.len()
            );
            if allowed.contains(&q.name) {
                waived.push(line);
            } else {
                failures.push(line);
            }
        }
        let ratio = |k: &str| qw[k].as_f64().unwrap() / qsolr[k].as_f64().unwrap();
        p50.push(ratio("p50"));
        p99.push(ratio("p99"));
        cpu.push(ratio("cpu_ms_per_1k"));
    }
    let geo = |v: &[f64]| (v.iter().map(|x| x.ln()).sum::<f64>() / v.len() as f64).exp();
    let mem = wf["mem_peak_mb"].as_f64().unwrap() / solr["mem_peak_mb"].as_f64().unwrap();
    let score = geo(&[geo(&p50), geo(&p99), geo(&cpu), mem]);
    let n = qs.len();

    let env = machine::env_json(&EnvInfo {
        kernel: read("/proc/sys/kernel/osrelease").trim().to_string(),
        cpu_model: machine::parse_cpu_model(&read("/proc/cpuinfo")).unwrap_or_default(),
        engine_cpuset: CPUSET.into(),
        driver_cpuset: DRIVER_CPUSET.into(),
        wayfinder_git_sha: git_sha(&root),
        solr_version,
        solr_image: SOLR_IMAGE.into(),
        solr_heap: SOLR_HEAP.into(),
    });

    let out = bench_dir.join(format!("out/{ts}.json"));
    let report = json!({
        "ts": ts, "docs": n_docs, "iters": iters, "cpuset": CPUSET, "memory": MEMORY,
        "env": env, "load_1m": load, "busy_warning": busy_warning,
        "score": score, "failures": failures, "waived": waived, "solr": solr, "wayfinder": wf,
    });
    std::fs::write(&out, serde_json::to_string_pretty(&report).unwrap()).expect("write report");

    println!(
        "score {score:.3} (wf/solr, <1 = wf wins) | p50 {:.2} p99 {:.2} cpu {:.2} mem {mem:.2} | correct {}/{n} | {} | {}",
        geo(&p50),
        geo(&p99),
        geo(&cpu),
        n - failures.len(),
        if failures.is_empty() { "PASS" } else { "FAIL" },
        out.strip_prefix(&root).unwrap_or(&out).display(),
    );
    for w in &waived {
        println!("  waived {w}");
    }
    for f in &failures {
        println!("  FAIL {f}");
    }
    if !failures.is_empty() {
        std::process::exit(1);
    }
}
