//! `wf-bench`: Wayfinder vs Solr speed, cost, and correctness (issue #440).
//!
//! Runs each engine in its own Docker container with identical CPU and memory
//! limits and host networking, one engine at a time, so neither competes with
//! the other for cores. Indexes the same docs into both, runs the same queries,
//! and compares the answers. CPU and memory come from each container's cgroup,
//! so both engines are measured the same way.
//!
//! Stdout is one line, built for a hill-climb loop to read cheaply. Full detail
//! is written to `bench/out/<unix-ts>.json`.
//!
//! Usage: `wf-bench [--docs N] [--iters N]`
//! Requires: docker, a release build at `target/release/wayfinder`, and the
//! corpus from `bench/fetch-wiki.sh`.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use wayfinder_bench::check::check;
use wayfinder_bench::wiki::{self, WikiDoc};

// ponytail: fixed core set and memory cap for both engines. Phase 6 of
// BENCHMARK_PLAN.md makes these configurable and pins the load generator.
const CPUSET: &str = "2-5";
const MEMORY: &str = "4g";
const SOLR_HEAP: &str = "2g";
const SOLR_PORT: u16 = 18983;
const WF_PORT: u16 = 18984;
const CORE: &str = "content";
// Per-request byte cap for indexing. Article sizes vary a lot, so batches
// are cut by size, not count. Under Wayfinder's 10 MB default body limit.
const BATCH_BYTES: usize = 4_000_000;
// Warm-up per query: at least this many requests and this long, so Solr's
// JIT has compiled the hot path before anything is timed.
const WARMUP_MIN: usize = 200;
const WARMUP_TIME: Duration = Duration::from_secs(2);
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
    /// Already percent-encoded, ready to append to `/select?`.
    params: String,
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
                    format!("{k}={}", encode(v))
                })
                .collect::<Vec<_>>()
                .join("&");
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

fn start(name: &'static str, run_args: &[&str], base: String, ping: &str) -> (Engine, f64) {
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
    };
    let url = format!("{}/{ping}", engine.base);
    while ureq::get(&url).call().is_err() {
        assert!(
            t0.elapsed() < Duration::from_secs(120),
            "{name} never answered {url}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    (engine, t0.elapsed().as_secs_f64() * 1000.0)
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

/// Measures every query on one engine. Returns per-query stats plus the
/// answer from the first (cold) request, for the correctness check.
fn measure(engine: &Engine, queries: &[Query], iters: usize) -> Vec<Value> {
    queries
        .iter()
        .map(|q| {
            let url = format!("{}/select?{}&wt=json", engine.base, q.params);
            let (cold_ms, answer) = get(&url);
            let t0 = Instant::now();
            let mut n = 0;
            while n < WARMUP_MIN || t0.elapsed() < WARMUP_TIME {
                get(&url);
                n += 1;
            }
            let cpu0 = cgroup_u64(engine, "cpu.stat", Some("usage_usec"));
            let mut lat: Vec<f64> = (0..iters).map(|_| get(&url).0).collect();
            let cpu_us = cgroup_u64(engine, "cpu.stat", Some("usage_usec")) - cpu0;
            lat.sort_by(f64::total_cmp);
            json!({
                "query": q.name,
                "cold_ms": cold_ms,
                "p50": pct(&lat, 0.50),
                "p95": pct(&lat, 0.95),
                "p99": pct(&lat, 0.99),
                "cpu_ms_per_1k": cpu_us as f64 / iters as f64,
                "answer": answer,
            })
        })
        .collect()
}

fn run_engine(
    engine: Engine,
    startup_ms: f64,
    docs: &[WikiDoc],
    qs: &[Query],
    iters: usize,
) -> Value {
    let idle_rss = cgroup_u64(&engine, "memory.current", None);
    let docs_per_s = index(&engine, docs);
    let post_index_rss = cgroup_u64(&engine, "memory.current", None);
    let queries = measure(&engine, qs, iters);
    json!({
        "engine": engine.name,
        "startup_ms": startup_ms,
        "index_docs_per_s": docs_per_s,
        "mem_idle_mb": idle_rss as f64 / 1048576.0,
        "mem_post_index_mb": post_index_rss as f64 / 1048576.0,
        "mem_peak_mb": cgroup_u64(&engine, "memory.peak", None) as f64 / 1048576.0,
        "queries": queries,
    })
}

fn run_solr(docs: &[WikiDoc], qs: &[Query], iters: usize) -> Value {
    let port_env = format!("SOLR_PORT={SOLR_PORT}");
    let heap_env = format!("SOLR_HEAP={SOLR_HEAP}");
    let (engine, startup_ms) = start(
        "solr",
        &[
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
    run_engine(engine, startup_ms, docs, qs, iters)
}

fn run_wayfinder(root: &Path, work: &Path, docs: &[WikiDoc], qs: &[Query], iters: usize) -> Value {
    let bin = root.join("target/release/wayfinder");
    assert!(bin.exists(), "build first: cargo build --release");
    std::fs::write(work.join("schema.toml"), WF_SCHEMA).expect("write schema");
    let bin_mount = format!("{}:/wayfinder:ro", bin.display());
    let work_mount = format!("{}:/work", work.display());
    let user = format!(
        "{}:{}",
        docker_id("-u"),
        docker_id("-g") // run as the host user so the data dir stays removable
    );
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
    );
    run_engine(engine, startup_ms, docs, qs, iters)
}

fn docker_id(flag: &str) -> String {
    let out = Command::new("id").arg(flag).output().expect("id");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn arg(args: &[String], flag: &str, default: usize) -> usize {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .map_or(default, |v| v.parse().expect("numeric flag"))
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let n_docs = arg(&args, "--docs", 10_000);
    let iters = arg(&args, "--iters", 200);
    let bench_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let root = bench_dir.parent().expect("repo root").to_path_buf();
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
    let solr = run_solr(&docs, &qs, iters);
    let wf = run_wayfinder(&root, &work, &docs, &qs, iters);
    let _ = std::fs::remove_dir_all(&work);

    let mut failures = Vec::new();
    let mut waived = Vec::new();
    let mut p50 = Vec::new();
    let mut p99 = Vec::new();
    let mut cpu = Vec::new();
    let pairs = wf["queries"].as_array().unwrap().iter();
    for ((qw, qsolr), q) in pairs.zip(solr["queries"].as_array().unwrap()).zip(&qs) {
        if let Err(why) = check(&q.mode, &qw["answer"], &qsolr["answer"]) {
            let line = format!("{}: {why}", q.name);
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

    let out = bench_dir.join(format!("out/{ts}.json"));
    let report = json!({
        "ts": ts, "docs": n_docs, "iters": iters, "cpuset": CPUSET, "memory": MEMORY,
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
