//! Unix-only operational contract: the `snapshot` CLI copies a committed
//! live index without taking its serving process down.

#[cfg(unix)]
mod unix {
    use std::io::{ErrorKind, Read, Write};
    use std::net::{SocketAddr, TcpListener, TcpStream};
    use std::path::Path;
    use std::process::{Child, Command, Stdio};
    use std::sync::mpsc;
    use std::thread;
    use std::time::{Duration, Instant};

    use serde_json::Value;
    use tantivy::{Index, collector::Count, query::AllQuery};
    use tempfile::TempDir;

    const SCHEMA_TOML: &str = r#"
[core]
name = "online-snapshot"
unique_key = "id"
default_field = "body"

[[fields]]
name = "id"
type = "string"
stored = true
required = true

[[fields]]
name = "body"
type = "text_en"
stored = true
"#;

    fn unused_localhost_addr() -> SocketAddr {
        let listener =
            TcpListener::bind("127.0.0.1:0").expect("reserve an ephemeral localhost port");
        let addr = listener.local_addr().expect("read reserved port");
        drop(listener);
        addr
    }

    struct Server {
        child: Child,
    }

    impl Drop for Server {
        fn drop(&mut self) {
            match self.child.try_wait() {
                Ok(Some(_)) => {}
                Ok(None) => match self.child.kill() {
                    Ok(()) => {
                        if let Err(error) = self.child.wait() {
                            eprintln!("failed to wait for snapshot test server: {error}");
                        }
                    }
                    Err(error) => {
                        eprintln!("failed to terminate snapshot test server: {error}");
                    }
                },
                Err(error) => {
                    eprintln!("failed to inspect snapshot test server: {error}");
                }
            }
        }
    }

    fn start_server(schema: &Path, data_dir: &Path, addr: SocketAddr) -> Server {
        Server {
            child: Command::new(env!("CARGO_BIN_EXE_wayfinder"))
                .arg(schema)
                .arg(data_dir)
                .arg(addr.to_string())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("spawn wayfinder server"),
        }
    }

    fn http_request(addr: SocketAddr, method: &str, target: &str, body: &str) -> (u16, String) {
        let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(1))
            .unwrap_or_else(|error| panic!("connect to {addr}: {error}"));
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("set HTTP read timeout");
        let request = format!(
            "{method} {target} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        stream
            .write_all(request.as_bytes())
            .expect("write HTTP request");
        // Read the full response, retrying through the transient WouldBlock /
        // TimedOut reads that appear when the machine is under CPU contention
        // instead of treating the first slow read as fatal. On Unix a fired
        // read-timeout is surfaced as WouldBlock. The continuous updater thread
        // in the snapshot-under-load test relies on this: a panic here would
        // silently stop commits and cascade into the merge-observation timeout.
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut response = Vec::new();
        loop {
            let mut buf = [0u8; 8192];
            match stream.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => response.extend_from_slice(&buf[..n]),
                Err(ref error)
                    if error.kind() == ErrorKind::WouldBlock
                        || error.kind() == ErrorKind::TimedOut =>
                {
                    assert!(
                        Instant::now() < deadline,
                        "timed out reading HTTP response from {addr}; partial response: {:?}",
                        String::from_utf8_lossy(&response)
                    );
                    thread::sleep(Duration::from_millis(2));
                }
                Err(error) => panic!("read HTTP response from {addr}: {error}"),
            }
        }
        let response = String::from_utf8(response).expect("HTTP response is UTF-8");
        let (head, body) = response
            .split_once("\r\n\r\n")
            .unwrap_or_else(|| panic!("malformed HTTP response: {response:?}"));
        let status = head
            .split_whitespace()
            .nth(1)
            .expect("HTTP status")
            .parse::<u16>()
            .expect("numeric HTTP status");
        (status, body.to_owned())
    }

    fn snapshot_stats(data_dir: &Path) -> (usize, usize) {
        let index = Index::open_in_dir(data_dir).expect("open snapshot Tantivy index");
        let segment_count = index
            .searchable_segment_metas()
            .expect("read snapshot segments")
            .len();
        let reader = index.reader().expect("open snapshot reader");
        let doc_count = reader
            .searcher()
            .search(&AllQuery, &Count)
            .expect("count snapshot documents");
        (doc_count, segment_count)
    }

    fn snapshot_doc_count(data_dir: &Path) -> usize {
        snapshot_stats(data_dir).0
    }

    fn wait_until_serving(addr: SocketAddr) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Ok(mut stream) = TcpStream::connect_timeout(&addr, Duration::from_millis(100)) {
                let _ = stream.write_all(
                    b"GET /wayfinder/online-snapshot/admin/ping?wt=json HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
                );
                let mut response = String::new();
                if stream.read_to_string(&mut response).is_ok()
                    && response.starts_with("HTTP/1.1 200")
                {
                    return;
                }
            }
            assert!(
                Instant::now() < deadline,
                "wayfinder did not start listening on {addr}"
            );
            thread::sleep(Duration::from_millis(25));
        }
    }

    #[test]
    fn snapshot_cli_copies_a_committed_live_index_without_stopping_the_server() {
        let temp = TempDir::new().expect("create temporary test directory");
        let schema = temp.path().join("schema.toml");
        let live_data_dir = temp.path().join("live-data");
        let snapshot_data_dir = temp.path().join("snapshot-data");
        std::fs::write(&schema, SCHEMA_TOML).expect("write minimal schema");
        std::fs::create_dir(&live_data_dir).expect("create live data directory");

        let live_addr = unused_localhost_addr();
        let mut live_server = start_server(&schema, &live_data_dir, live_addr);
        wait_until_serving(live_addr);

        let (status, response) = http_request(
            live_addr,
            "POST",
            "/wayfinder/online-snapshot/update?commit=true&wt=json",
            r#"[{"id":"snapshot-doc","body":"committed while online"}]"#,
        );
        assert_eq!(status, 200, "committed update must succeed: {response}");

        let live_synonyms = live_data_dir.join("synonyms.txt");
        std::fs::write(&live_synonyms, "trail,path\n").expect("create durable live synonym state");
        assert!(live_synonyms.is_file(), "live synonym state must exist");

        let snapshot = Command::new(env!("CARGO_BIN_EXE_wayfinder"))
            .arg("snapshot")
            .arg(&live_data_dir)
            .arg(&snapshot_data_dir)
            .output()
            .expect("run snapshot CLI");
        assert!(
            snapshot.status.success(),
            "snapshot CLI must succeed while the source server is running; stdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&snapshot.stdout),
            String::from_utf8_lossy(&snapshot.stderr),
        );
        assert!(
            matches!(live_server.child.try_wait(), Ok(None)),
            "snapshot must not stop the live server"
        );
        assert!(
            !snapshot_data_dir.join("synonyms.txt").exists(),
            "online index snapshot must omit durable synonym state"
        );
        let (status, response) = http_request(
            live_addr,
            "GET",
            "/wayfinder/online-snapshot/select?q=id:snapshot-doc&fl=id&wt=json",
            "",
        );
        assert_eq!(status, 200, "live server must remain queryable: {response}");

        let snapshot_addr = unused_localhost_addr();
        let _snapshot_server = start_server(&schema, &snapshot_data_dir, snapshot_addr);
        wait_until_serving(snapshot_addr);
        let (status, response) = http_request(
            snapshot_addr,
            "GET",
            "/wayfinder/online-snapshot/select?q=id:snapshot-doc&fl=id&wt=json",
            "",
        );
        assert_eq!(
            status, 200,
            "snapshot server must accept select: {response}"
        );
        let response: Value = serde_json::from_str(&response).expect("snapshot select is JSON");
        assert_eq!(
            response
                .pointer("/response/numFound")
                .and_then(Value::as_u64),
            Some(1),
            "snapshot must reopen with the committed document: {response}"
        );
        assert_eq!(
            response
                .pointer("/response/docs/0/id")
                .and_then(Value::as_str),
            Some("snapshot-doc"),
            "snapshot must preserve the committed document: {response}"
        );

        let marker = snapshot_data_dir.join("operator-marker");
        std::fs::write(&marker, "must survive").expect("write destination marker");
        let repeated = Command::new(env!("CARGO_BIN_EXE_wayfinder"))
            .arg("snapshot")
            .arg(&live_data_dir)
            .arg(&snapshot_data_dir)
            .output()
            .expect("rerun snapshot CLI against existing destination");
        assert!(
            !repeated.status.success(),
            "existing destination must be rejected"
        );
        assert_eq!(
            std::fs::read_to_string(marker).expect("read destination marker"),
            "must survive",
            "snapshot must not merge with or overwrite an existing destination"
        );
    }

    #[test]
    fn repeated_snapshots_reopen_during_continuous_commits_and_merges() {
        let temp = TempDir::new().expect("create temporary test directory");
        let schema = temp.path().join("schema.toml");
        let live_data_dir = temp.path().join("live-data");
        std::fs::write(&schema, SCHEMA_TOML).expect("write minimal schema");
        std::fs::create_dir(&live_data_dir).expect("create live data directory");

        let live_addr = unused_localhost_addr();
        let _live_server = start_server(&schema, &live_data_dir, live_addr);
        wait_until_serving(live_addr);
        let (status, response) = http_request(
            live_addr,
            "POST",
            "/wayfinder/online-snapshot/update?commit=true&wt=json",
            r#"[{"id":"seed","body":"seed generation"}]"#,
        );
        assert_eq!(status, 200, "seed commit must succeed: {response}");

        let (first_wave_tx, first_wave_rx) = mpsc::channel();
        let (resume_tx, resume_rx) = mpsc::channel();
        let updater = thread::spawn(move || {
            for batch in 0..30 {
                let body = "x".repeat(4096);
                let docs: Vec<Value> = (0..80)
                    .map(|doc| {
                        serde_json::json!({
                            "id": format!("batch-{batch}-doc-{doc}"),
                            "body": format!("{batch}-{doc}-{body}"),
                        })
                    })
                    .collect();
                let request = serde_json::to_string(&docs).expect("serialize committed batch");
                let (status, response) = http_request(
                    live_addr,
                    "POST",
                    "/wayfinder/online-snapshot/update?commit=true&wt=json",
                    &request,
                );
                assert_eq!(status, 200, "batch {batch} commit must succeed: {response}");
                if batch == 9 {
                    first_wave_tx.send(()).expect("announce first commit wave");
                    resume_rx.recv().expect("resume remaining commits");
                }
                thread::sleep(Duration::from_millis(2));
            }
        });

        let final_count = 1 + 30 * 80;
        let mut observed_counts = Vec::new();
        let mut attempt = 0;
        let mut first_wave_committed = false;
        // Hard cap so a stalled updater (no first-wave commit observed) cannot
        // hang the test; the merge window below is the meaningful budget.
        let outer_deadline = Instant::now() + Duration::from_secs(45);
        // Measure the merge wait from when the first commit wave lands, not from
        // loop start: under CPU contention the updater can spend most of a
        // fixed-from-start budget committing before Tantivy has segments to merge.
        let merge_window = Duration::from_secs(20);
        let mut merge_deadline: Option<Instant> = None;
        loop {
            let destination = temp.path().join(format!("snapshot-{attempt}"));
            let snapshot = Command::new(env!("CARGO_BIN_EXE_wayfinder"))
                .arg("snapshot")
                .arg(&live_data_dir)
                .arg(&destination)
                .output()
                .expect("run snapshot CLI under indexing load");
            assert!(
                snapshot.status.success(),
                "snapshot {attempt} must survive concurrent commit/merge/GC; stderr:\n{}",
                String::from_utf8_lossy(&snapshot.stderr)
            );
            let (count, segment_count) = snapshot_stats(&destination);
            assert!(
                (1..=final_count).contains(&count) && (count - 1) % 80 == 0,
                "snapshot {attempt} must contain the seed plus whole 80-document commits, got {count} docs"
            );
            observed_counts.push((count, segment_count));
            attempt += 1;

            let just_landed_first_wave = !first_wave_committed && first_wave_rx.try_recv().is_ok();
            first_wave_committed |= just_landed_first_wave;
            if just_landed_first_wave {
                merge_deadline = Some(Instant::now() + merge_window);
            }
            let committed_batches = (count - 1) / 80;
            if first_wave_committed
                && committed_batches >= 8
                && segment_count < committed_batches + 1
            {
                break;
            }
            if Instant::now() >= merge_deadline.unwrap_or(outer_deadline) {
                let _ = resume_tx.send(());
                panic!(
                    "a snapshot did not observe Tantivy's first-wave merge before the deadline: {observed_counts:?}"
                );
            }
        }

        resume_tx.send(()).expect("resume remaining commits");
        for _ in 0..20 {
            let destination = temp.path().join(format!("snapshot-{attempt}"));
            let snapshot = Command::new(env!("CARGO_BIN_EXE_wayfinder"))
                .arg("snapshot")
                .arg(&live_data_dir)
                .arg(&destination)
                .output()
                .expect("run snapshot CLI under indexing load");
            assert!(
                snapshot.status.success(),
                "snapshot {attempt} must survive concurrent commit/merge/GC; stderr:\n{}",
                String::from_utf8_lossy(&snapshot.stderr)
            );
            let (count, segment_count) = snapshot_stats(&destination);
            assert!(
                (1..=final_count).contains(&count) && (count - 1) % 80 == 0,
                "snapshot {attempt} must contain the seed plus whole 80-document commits, got {count} docs"
            );
            observed_counts.push((count, segment_count));
            attempt += 1;
        }
        updater.join().expect("continuous updater must finish");

        assert!(
            observed_counts
                .iter()
                .any(|&(count, _)| count < final_count),
            "at least one snapshot must overlap indexing rather than all running afterward: {observed_counts:?}"
        );
        assert_eq!(
            snapshot_doc_count(&live_data_dir),
            final_count,
            "all concurrent committed batches must remain durable"
        );
        let (status, response) = http_request(
            live_addr,
            "GET",
            "/wayfinder/online-snapshot/admin/ping?wt=json",
            "",
        );
        assert_eq!(status, 200, "live server must remain available: {response}");
    }
}
