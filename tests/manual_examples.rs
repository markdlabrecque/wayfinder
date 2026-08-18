//! The retained executable quickstart for issue #423.
//!
//! This deliberately drives the shipped binary over loopback HTTP rather than
//! an in-process router. Its schema and corpus are the manual's canonical
//! files. QTime is the only volatile response field, so it is never compared;
//! every asserted value below is stable user-visible behavior.

#[cfg(unix)]
mod unix {
    use std::io::{Read, Write};
    use std::net::{SocketAddr, TcpListener, TcpStream};
    use std::path::Path;
    use std::process::{Child, Command, ExitStatus, Stdio};
    use std::thread;
    use std::time::{Duration, Instant};

    use serde_json::{Value, json};
    use tempfile::TempDir;

    const CORE: &str = "getting-started";
    const QUICKSTART_MD: &str = include_str!("../manual/getting-started/quickstart.md");
    const SCHEMA_TOML: &str = include_str!("../manual/getting-started/schema.toml");
    const CORPUS_JSON: &str = include_str!("../manual/getting-started/corpus.json");

    fn unused_loopback_addr() -> SocketAddr {
        let listener =
            TcpListener::bind("127.0.0.1:0").expect("reserve an ephemeral loopback port");
        let addr = listener
            .local_addr()
            .expect("read reserved loopback address");
        drop(listener);
        addr
    }

    struct Server {
        child: Child,
    }

    impl Drop for Server {
        fn drop(&mut self) {
            if matches!(self.child.try_wait(), Ok(None)) {
                let _ = Command::new("kill")
                    .args(["-TERM", &self.child.id().to_string()])
                    .status();
                let _ = self.child.wait();
            }
        }
    }

    fn start_server(schema: &Path, data_dir: &Path, addr: SocketAddr, log: &Path) -> Server {
        let log = std::fs::File::create(log).expect("create/truncate quickstart server log");
        Server {
            child: Command::new(env!("CARGO_BIN_EXE_wayfinder"))
                .env_remove("WAYFINDER_CONFIG")
                .env("RUST_LOG", "info")
                .arg(schema)
                .arg(data_dir)
                .arg(addr.to_string())
                .stdout(Stdio::null())
                .stderr(log)
                .spawn()
                .expect("spawn the Wayfinder binary"),
        }
    }

    fn http_request(
        addr: SocketAddr,
        method: &str,
        target: &str,
        content_type: Option<&str>,
        body: &str,
    ) -> (u16, String, String) {
        let mut stream = TcpStream::connect_timeout(&addr, Duration::from_millis(250))
            .unwrap_or_else(|error| panic!("connect to {addr}: {error}"));
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("set HTTP read timeout");

        let content_type = content_type
            .map(|value| format!("Content-Type: {value}\r\n"))
            .unwrap_or_default();
        let request = format!(
            "{method} {target} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n{content_type}Content-Length: {}\r\n\r\n{body}",
            body.len()
        );
        stream
            .write_all(request.as_bytes())
            .expect("write HTTP request");

        let mut response = String::new();
        stream
            .read_to_string(&mut response)
            .expect("read HTTP response");
        let (head, body) = response
            .split_once("\r\n\r\n")
            .unwrap_or_else(|| panic!("malformed HTTP response: {response:?}"));
        let status = head
            .split_whitespace()
            .nth(1)
            .expect("HTTP status")
            .parse()
            .expect("numeric HTTP status");
        (status, head.to_owned(), body.to_owned())
    }

    fn wait_until_serving(server: &mut Server, addr: SocketAddr, log: &Path) -> bool {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if matches!(server.child.try_wait(), Ok(Some(_))) {
                return false;
            }
            let child_bound = std::fs::read_to_string(log)
                .is_ok_and(|contents| contents.contains("wayfinder listening"));
            if child_bound
                && let Ok(mut stream) =
                    TcpStream::connect_timeout(&addr, Duration::from_millis(100))
            {
                stream
                    .set_read_timeout(Some(Duration::from_millis(250)))
                    .expect("bound readiness-probe read");
                let _ = stream.write_all(
                    format!(
                        "GET /wayfinder/{CORE}/admin/ping?wt=json HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n"
                    )
                    .as_bytes(),
                );
                let mut response = String::new();
                if stream.read_to_string(&mut response).is_ok()
                    && response.starts_with("HTTP/1.1 200")
                {
                    return true;
                }
            }
            assert!(
                Instant::now() < deadline,
                "Wayfinder child did not log a successful bind and serve on {addr}"
            );
            thread::sleep(Duration::from_millis(25));
        }
    }

    fn start_server_with_retry(schema: &Path, data_dir: &Path, log: &Path) -> (Server, SocketAddr) {
        for _ in 0..5 {
            let addr = unused_loopback_addr();
            let mut server = start_server(schema, data_dir, addr, log);
            if wait_until_serving(&mut server, addr, log) {
                return (server, addr);
            }
            let failure = std::fs::read_to_string(log)
                .unwrap_or_else(|error| format!("could not read child log: {error}"));
            assert!(
                failure.contains("Address already in use")
                    && !failure.contains("wayfinder listening"),
                "retry is allowed only for a pre-bind address collision; child log: {failure}"
            );
        }
        panic!(
            "Wayfinder repeatedly failed before binding: {}",
            std::fs::read_to_string(log).unwrap_or_else(|error| error.to_string())
        );
    }

    fn sigterm(server: &mut Server) -> ExitStatus {
        let status = Command::new("kill")
            .args(["-TERM", &server.child.id().to_string()])
            .status()
            .expect("send SIGTERM to Wayfinder");
        assert!(status.success(), "kill -TERM must succeed: {status}");
        server
            .child
            .wait()
            .expect("wait for graceful Wayfinder exit")
    }

    fn json_response(status: u16, headers: &str, body: String, route: &str) -> Value {
        assert_eq!(status, 200, "{route} must return HTTP 200: {body}");
        assert!(
            headers
                .to_ascii_lowercase()
                .contains("content-type: application/json"),
            "{route} must return JSON, got headers: {headers}"
        );
        serde_json::from_str(&body).unwrap_or_else(|error| {
            panic!("{route} must return valid JSON: {error}; response: {body}")
        })
    }

    fn html_response(status: u16, headers: &str, body: String, route: &str) -> String {
        assert_eq!(status, 200, "{route} must return HTTP 200: {body}");
        assert!(
            headers
                .to_ascii_lowercase()
                .contains("content-type: text/html"),
            "{route} must return HTML, got headers: {headers}"
        );
        body
    }

    fn has_facet_pair(values: &[Value], term: &str, count: u64) -> bool {
        values
            .chunks_exact(2)
            .any(|pair| pair[0] == term && pair[1].as_u64() == Some(count))
    }

    #[test]
    fn canonical_quickstart_runs_a_real_binary_through_restart() {
        for fragment in [
            "\"$BASE/admin/ping?wt=json\"",
            "\"$BASE/update?commit=true&wt=json\"",
            "'q=trail'",
            "'fq=category:guides'",
            "'fl=id,title,rank'",
            "'sort=rank asc'",
            "'start=1'",
            "'rows=1'",
            "'facet=true'",
            "'facet.field=category'",
            "one `filters` document in `response.docs`",
            "`response.numFound`\nis `2`",
            "`facet_counts` includes the `guides` category",
            "\"http://$LISTENER/ui\"",
            "\"http://$LISTENER/ui/query\"",
            "\"http://$LISTENER/ui/stats\"",
            "'q=*:*'",
            "response.numFound: 3",
        ] {
            assert!(
                QUICKSTART_MD.contains(fragment),
                "the real-binary request/expectation must remain coupled to the documented fragment {fragment:?}"
            );
        }

        let corpus: Value = serde_json::from_str(CORPUS_JSON).expect("canonical corpus is JSON");
        assert_eq!(
            corpus,
            json!([
                {"id":"intro","title":"Trail map introduction","body":"Follow a trail from a small corpus to a searchable index.","category":"guides","rank":10},
                {"id":"filters","title":"Trail filter guide","body":"Filter a trail query with an exact category field.","category":"guides","rank":20},
                {"id":"reference","title":"Reference fields","body":"Reference material describes stored and fast fields.","category":"reference","rank":30}
            ]),
            "the manual corpus is a deterministic worked example"
        );

        let temp = TempDir::new().expect("create temporary manual workspace");
        let schema = temp.path().join("schema.toml");
        let data_dir = temp.path().join("data");
        std::fs::write(&schema, SCHEMA_TOML).expect("copy canonical schema into workspace");
        std::fs::create_dir(&data_dir).expect("create manual data directory");

        let log = temp.path().join("server.log");
        let (mut server, addr) = start_server_with_retry(&schema, &data_dir, &log);

        let (status, headers, body) = http_request(
            addr,
            "GET",
            &format!("/wayfinder/{CORE}/admin/ping?wt=json"),
            None,
            "",
        );
        let ping = json_response(status, &headers, body, "admin ping");
        assert_eq!(ping["status"], "OK");

        let (status, headers, body) = http_request(
            addr,
            "POST",
            &format!("/wayfinder/{CORE}/update?commit=true&wt=json"),
            Some("application/json"),
            CORPUS_JSON,
        );
        let update = json_response(status, &headers, body, "canonical corpus update");
        assert_eq!(update["responseHeader"]["status"], 0);

        let (status, headers, body) = http_request(
            addr,
            "GET",
            &format!(
                "/wayfinder/{CORE}/select?q=trail&fq=category%3Aguides&fl=id%2Ctitle%2Crank&sort=rank%20asc&start=1&rows=1&facet=true&facet.field=category&wt=json"
            ),
            None,
            "",
        );
        let select = json_response(status, &headers, body, "filtered, paged select");
        assert_eq!(select["response"]["numFound"], 2);
        assert_eq!(select["response"]["start"], 1);
        assert_eq!(
            select["response"]["docs"],
            json!([{"id":"filters","title":"Trail filter guide","rank":20}]),
            "fl, sort, and page must produce the quickstart's one stable result"
        );
        let facet = select["facet_counts"]["facet_fields"]["category"]
            .as_array()
            .expect("category facet must be a flat alternating array");
        assert!(
            has_facet_pair(facet, "guides", 2),
            "the filtered category facet must count the two guide documents: {select}"
        );

        for (route, expected) in [
            ("/ui", "getting-started"),
            ("/ui/query?q=trail&rows=1&wt=json", "filters"),
            ("/ui/stats", "Documents"),
        ] {
            let (status, headers, body) = http_request(addr, "GET", route, None, "");
            let page = html_response(status, &headers, body, route);
            assert!(
                page.contains(expected),
                "{route} must render {expected:?}: {page}"
            );
        }

        let exit = sigterm(&mut server);
        assert!(
            exit.success(),
            "SIGTERM must stop the quickstart server cleanly: {exit}"
        );

        let (mut restarted, restart_addr) = start_server_with_retry(&schema, &data_dir, &log);
        let (status, headers, body) = http_request(
            restart_addr,
            "GET",
            &format!("/wayfinder/{CORE}/select?q=*:*&rows=0&wt=json"),
            None,
            "",
        );
        let persisted = json_response(status, &headers, body, "select after restart");
        assert_eq!(
            persisted["response"]["numFound"], 3,
            "all committed canonical documents must survive a restart using the same data directory"
        );

        let exit = sigterm(&mut restarted);
        assert!(
            exit.success(),
            "the restarted quickstart server must stop cleanly: {exit}"
        );
    }
}
