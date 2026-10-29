//! Issue #451: index size on disk and time from start to the first successful
//! search, recorded for both engines by `wf-bench`.
//!
//! Docker is side-effectful, so these hermetic tests pin the seams around it:
//! summing a data dir, and polling a search URL until it first answers 200.

mod support;

use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use support::fresh_scratch_dir;
use wayfinder_bench::footprint::{
    dir_size_bytes, solr_index_dir, time_to_first_search, wayfinder_index_dir,
};

fn write(path: &Path, bytes: usize) {
    fs::create_dir_all(path.parent().unwrap()).expect("create parent");
    fs::write(path, vec![b'x'; bytes]).expect("write file");
}

/// Serves one canned status per connection, in order, then 200 forever.
/// Returns the base URL and the number of requests seen so far via the join.
fn serve(statuses: Vec<u16>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().unwrap();
    thread::spawn(move || {
        let mut statuses = statuses.into_iter();
        for stream in listener.incoming() {
            let mut stream = stream.expect("accept");
            let mut buf = [0u8; 2048];
            let _ = stream.read(&mut buf);
            let status = statuses.next().unwrap_or(200);
            let resp = format!(
                "HTTP/1.1 {status} X\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}"
            );
            let _ = stream.write_all(resp.as_bytes());
        }
    });
    format!("http://{addr}/select?q=*:*&wt=json")
}

#[test]
fn index_size_sums_every_file_including_nested_segments() {
    let dir = fresh_scratch_dir("size-sum");
    write(&dir.join("meta.json"), 100);
    write(&dir.join("segments/a.term"), 10_000);
    write(&dir.join("segments/deep/b.store"), 1);
    assert_eq!(dir_size_bytes(&dir).expect("size"), 10_101);
}

#[test]
fn index_size_of_an_empty_dir_is_zero() {
    let dir = fresh_scratch_dir("size-empty");
    assert_eq!(dir_size_bytes(&dir).expect("size"), 0);
}

#[test]
fn index_size_of_a_missing_dir_is_an_error_not_zero() {
    let dir = fresh_scratch_dir("size-missing");
    assert!(dir_size_bytes(&dir.join("nope")).is_err());
}

#[test]
fn first_search_time_is_measured_from_the_given_start_instant() {
    let url = serve(vec![]);
    let since = Instant::now() - Duration::from_millis(500);
    let ms = time_to_first_search(&url, since, Duration::from_secs(5)).expect("first search");
    assert!(ms >= 500.0, "must include time before the call, got {ms}");
    assert!(
        ms < 5_000.0,
        "an immediately-ready server is fast, got {ms}"
    );
}

#[test]
fn first_search_waits_out_non_200_answers() {
    let url = serve(vec![503, 503, 404]);
    let since = Instant::now();
    let ms = time_to_first_search(&url, since, Duration::from_secs(10)).expect("first search");
    assert!(ms > 0.0);
}

#[test]
fn first_search_retries_while_nothing_is_listening_then_succeeds() {
    // Reserve a port, free it, and only start serving after a delay.
    let port = {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    thread::spawn(move || {
        thread::sleep(Duration::from_millis(300));
        let listener = TcpListener::bind(("127.0.0.1", port)).expect("rebind");
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            let mut buf = [0u8; 2048];
            let _ = stream.read(&mut buf);
            let _ = stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}");
        }
    });
    let url = format!("http://127.0.0.1:{port}/select?q=*:*");
    let ms =
        time_to_first_search(&url, Instant::now(), Duration::from_secs(10)).expect("first search");
    assert!(ms >= 300.0, "cannot beat the server's own start, got {ms}");
}

#[test]
fn first_search_that_never_succeeds_times_out_with_an_error() {
    let url = serve(vec![503; 10_000]);
    let r = time_to_first_search(&url, Instant::now(), Duration::from_millis(300));
    assert!(r.is_err(), "a server that never returns 200 is an error");
}

// `wf-bench` sizes whatever `*_index_dir` returns, so a wrong path here is a
// wrong index size in every result (#482).

#[test]
fn solr_index_size_counts_the_lucene_index_and_not_the_transaction_log() {
    let work = fresh_scratch_dir("solr-layout");
    let core = work.join("solr/data/content/data");
    write(&core.join("index/_0.cfs"), 4_000);
    write(&core.join("index/segments_1"), 100);
    write(&core.join("tlog/tlog.0000000000000000001"), 900_000);
    write(&work.join("solr/data/content/core.properties"), 50);

    let dir = solr_index_dir(&work, "content");
    assert_eq!(dir_size_bytes(&dir).expect("size"), 4_100);
}

#[test]
fn solr_index_dir_follows_the_core_name() {
    let work = fresh_scratch_dir("solr-core-name");
    write(&work.join("solr/data/other/data/index/_0.cfs"), 7);
    assert_eq!(
        dir_size_bytes(&solr_index_dir(&work, "other")).expect("size"),
        7
    );
    assert!(dir_size_bytes(&solr_index_dir(&work, "content")).is_err());
}

#[test]
fn wayfinder_index_size_counts_everything_in_its_data_dir() {
    let work = fresh_scratch_dir("wf-layout");
    write(&work.join("data/meta.json"), 100);
    write(&work.join("data/seg/a.term"), 2_000);
    write(&work.join("schema.toml"), 999);

    let dir = wayfinder_index_dir(&work);
    assert_eq!(dir_size_bytes(&dir).expect("size"), 2_100);
}
