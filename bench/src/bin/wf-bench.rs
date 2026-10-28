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
const WARMUP: usize = 20;
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
    name: &'static str,
    params: &'static str,
}

const QUERIES: &[Query] = &[Query {
    name: "edismax_term",
    params: "q=history&defType=edismax&qf=title+body&rows=10&fl=id",
}];

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
fn measure(engine: &Engine, iters: usize) -> Vec<Value> {
    QUERIES
        .iter()
        .map(|q| {
            let url = format!("{}/select?{}&wt=json", engine.base, q.params);
            let (cold_ms, answer) = get(&url);
            for _ in 0..WARMUP {
                get(&url);
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

fn run_engine(engine: Engine, startup_ms: f64, docs: &[WikiDoc], iters: usize) -> Value {
    let idle_rss = cgroup_u64(&engine, "memory.current", None);
    let docs_per_s = index(&engine, docs);
    let post_index_rss = cgroup_u64(&engine, "memory.current", None);
    let queries = measure(&engine, iters);
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

fn run_solr(docs: &[WikiDoc], iters: usize) -> Value {
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
    run_engine(engine, startup_ms, docs, iters)
}

fn run_wayfinder(root: &Path, work: &Path, docs: &[WikiDoc], iters: usize) -> Value {
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
    run_engine(engine, startup_ms, docs, iters)
}

fn docker_id(flag: &str) -> String {
    let out = Command::new("id").arg(flag).output().expect("id");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn ids(answer: &Value) -> Vec<String> {
    answer["response"]["docs"]
        .as_array()
        .map(|docs| docs.iter().map(|d| d["id"].to_string()).collect())
        .unwrap_or_default()
}

/// Answers "make sense" when the hit count matches and the relevance-ranked
/// top results mostly agree. Exact order is not required: scoring differs.
fn check(wf: &Value, solr: &Value) -> Option<String> {
    let (nw, ns) = (&wf["response"]["numFound"], &solr["response"]["numFound"]);
    if nw != ns {
        return Some(format!("numFound wf={nw} solr={ns}"));
    }
    let (iw, is) = (ids(wf), ids(solr));
    let overlap = iw.iter().filter(|id| is.contains(id)).count();
    let need = (is.len() as f64 * 0.8).ceil() as usize;
    (overlap < need).then(|| format!("top-{} overlap {overlap}", is.len()))
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

    let docs = wiki::load(&bench_dir.join("data"), n_docs);
    let solr = run_solr(&docs, iters);
    let wf = run_wayfinder(&root, &work, &docs, iters);
    let _ = std::fs::remove_dir_all(&work);

    let mut failures = Vec::new();
    let mut p50 = Vec::new();
    let mut p99 = Vec::new();
    let mut cpu = Vec::new();
    for (qw, qs) in wf["queries"]
        .as_array()
        .unwrap()
        .iter()
        .zip(solr["queries"].as_array().unwrap())
    {
        if let Some(why) = check(&qw["answer"], &qs["answer"]) {
            failures.push(format!("{}: {why}", qw["query"].as_str().unwrap()));
        }
        let ratio = |k: &str| qw[k].as_f64().unwrap() / qs[k].as_f64().unwrap();
        p50.push(ratio("p50"));
        p99.push(ratio("p99"));
        cpu.push(ratio("cpu_ms_per_1k"));
    }
    let geo = |v: &[f64]| (v.iter().map(|x| x.ln()).sum::<f64>() / v.len() as f64).exp();
    let mem = wf["mem_peak_mb"].as_f64().unwrap() / solr["mem_peak_mb"].as_f64().unwrap();
    let score = geo(&[geo(&p50), geo(&p99), geo(&cpu), mem]);
    let n = QUERIES.len();

    let out = bench_dir.join(format!("out/{ts}.json"));
    let report = json!({
        "ts": ts, "docs": n_docs, "iters": iters, "cpuset": CPUSET, "memory": MEMORY,
        "score": score, "failures": failures, "solr": solr, "wayfinder": wf,
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
    for f in &failures {
        println!("  FAIL {f}");
    }
    if !failures.is_empty() {
        std::process::exit(1);
    }
}
