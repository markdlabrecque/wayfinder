//! Add-to-searchable time (issue #450). Runs the measurement against a fake
//! engine that makes each added doc findable a fixed delay after it was
//! posted, so the recorded times have a known floor.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use wayfinder_bench::freshness;

const VISIBLE_AFTER: Duration = Duration::from_millis(60);

#[derive(Default)]
struct Seen {
    /// Body of every POST to /update, with when it arrived.
    added: Vec<(String, Instant)>,
    /// Distinct `q` values polled, in order of first sight.
    queries: Vec<String>,
}

fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let (mut out, mut i) = (Vec::new(), 0);
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len() => {
                out.push(u8::from_str_radix(&s[i + 1..i + 3], 16).unwrap_or(b'?'));
                i += 3;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn handle(mut stream: TcpStream, seen: &Mutex<Seen>) {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let (head_end, len) = loop {
        let n = stream.read(&mut chunk).unwrap_or(0);
        if n == 0 {
            return;
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&buf[..p]).to_lowercase();
            let len = head
                .lines()
                .find_map(|l| l.strip_prefix("content-length:")?.trim().parse().ok())
                .unwrap_or(0usize);
            break (p + 4, len);
        }
    };
    while buf.len() < head_end + len {
        let n = stream.read(&mut chunk).unwrap_or(0);
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let text = String::from_utf8_lossy(&buf).into_owned();
    let request_line = text.lines().next().unwrap_or("").to_string();
    let body = String::from_utf8_lossy(&buf[head_end..]).into_owned();
    let num_found = if request_line.contains("/update") {
        seen.lock().unwrap().added.push((body, Instant::now()));
        0
    } else if request_line.contains("/select") {
        let target = request_line.split_whitespace().nth(1).unwrap_or("");
        let q = target
            .split(['?', '&'])
            .find_map(|kv| kv.strip_prefix("q="))
            .map(decode)
            .unwrap_or_default();
        let token = q.trim_matches('"').rsplit(':').next().unwrap().trim_matches('"').to_string();
        let mut s = seen.lock().unwrap();
        if !s.queries.contains(&q) {
            s.queries.push(q);
        }
        let hit = s
            .added
            .iter()
            .any(|(b, at)| b.contains(&token) && at.elapsed() >= VISIBLE_AFTER);
        hit as u64
    } else {
        0
    };
    let json = format!(r#"{{"responseHeader":{{"status":0}},"response":{{"numFound":{num_found},"docs":[]}}}}"#);
    let _ = write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{json}",
        json.len()
    );
}

fn fake_engine() -> (String, Arc<Mutex<Seen>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}/solr/content", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Seen::default()));
    let shared = Arc::clone(&seen);
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let shared = Arc::clone(&shared);
            std::thread::spawn(move || handle(stream, &shared));
        }
    });
    (base, seen)
}

#[test]
fn records_one_time_per_run_that_covers_the_wait_until_searchable() {
    let (base, seen) = fake_engine();
    let report = freshness::measure(&base, 5, "autoCommit 15s, no explicit commit");

    let samples = report["samples_ms"].as_array().expect("samples_ms array");
    assert_eq!(samples.len(), 5);
    for s in samples {
        let ms = s.as_f64().expect("number");
        assert!(ms >= 60.0, "doc was not findable yet, yet recorded {ms} ms");
        assert!(ms < 5_000.0, "recorded {ms} ms, far above the real wait");
    }
    let p50 = report["p50_ms"].as_f64().expect("p50_ms");
    let p95 = report["p95_ms"].as_f64().expect("p95_ms");
    assert!(p50 >= 60.0 && p50 <= p95, "p50 {p50}, p95 {p95}");

    assert_eq!(seen.lock().unwrap().added.len(), 5, "one add per run");
}

#[test]
fn every_run_searches_for_its_own_unique_token() {
    let (base, seen) = fake_engine();
    freshness::measure(&base, 4, "default");
    let s = seen.lock().unwrap();
    assert_eq!(s.queries.len(), 4, "queries: {:?}", s.queries);
}

#[test]
fn commit_settings_are_written_into_the_report() {
    let (base, _seen) = fake_engine();
    let report = freshness::measure(&base, 2, "autoSoftCommit 1s");
    assert_eq!(report["commit_settings"], "autoSoftCommit 1s");
}
