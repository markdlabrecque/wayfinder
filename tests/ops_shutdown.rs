//! Unix-only operational contract: an acknowledged deferred update survives
//! SIGTERM even though its `commitWithin` deadline has not elapsed.

#[cfg(unix)]
mod unix {
    use std::io::{Read, Write};
    use std::net::{SocketAddr, TcpListener, TcpStream};
    use std::path::Path;
    use std::process::{Child, Command, ExitStatus, Stdio};
    use std::thread;
    use std::time::{Duration, Instant};

    use serde_json::Value;
    use tempfile::TempDir;

    const SCHEMA_TOML: &str = r#"
[core]
name = "shutdown"
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
            if matches!(self.child.try_wait(), Ok(None)) {
                let _ = Command::new("kill")
                    .args(["-TERM", &self.child.id().to_string()])
                    .status();
                let _ = self.child.wait();
            }
        }
    }

    fn start_server(schema: &Path, data_dir: &Path, addr: SocketAddr) -> Server {
        Server {
            child: Command::new(env!("CARGO_BIN_EXE_wayfinder"))
                .arg(schema)
                .arg(data_dir)
                .arg(addr.to_string())
                .env("RUST_LOG", "wayfinder=debug")
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()
                .expect("spawn wayfinder binary"),
        }
    }

    fn http_request(addr: SocketAddr, method: &str, target: &str, body: &str) -> (u16, String) {
        let mut stream = TcpStream::connect_timeout(&addr, Duration::from_millis(200))
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
        (status, body.to_owned())
    }

    /// The only polling in this test: bounded startup readiness polling.
    fn wait_until_serving(addr: SocketAddr) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Ok(mut stream) = TcpStream::connect_timeout(&addr, Duration::from_millis(100)) {
                let _ = stream.write_all(
                    b"GET /wayfinder/shutdown/admin/ping?wt=json HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
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

    /// SIGTERM is a Unix signal; using the system `kill` command keeps this
    /// integration test free of a platform-specific signal crate.
    fn sigterm(server: &mut Server) -> ExitStatus {
        Command::new("kill")
            .args(["-TERM", &server.child.id().to_string()])
            .status()
            .expect("run kill -TERM")
    }

    #[test]
    fn sigterm_flushes_an_acknowledged_deferred_update_before_clean_exit() {
        let temp = TempDir::new().expect("create temporary schema/data directory");
        let schema = temp.path().join("schema.toml");
        let data_dir = temp.path().join("data");
        std::fs::write(&schema, SCHEMA_TOML).expect("write minimal schema");
        std::fs::create_dir(&data_dir).expect("create data directory");

        let addr = unused_localhost_addr();
        let mut server = start_server(&schema, &data_dir, addr);
        wait_until_serving(addr);

        let document = r#"[{"id":"shutdown-doc","body":"durable after termination"}]"#;
        let (status, response) = http_request(
            addr,
            "POST",
            "/wayfinder/shutdown/update?commitWithin=60000&wt=json&ignored_by_wayfinder=yes",
            document,
        );
        assert_eq!(
            status, 200,
            "deferred update must be acknowledged: {response}"
        );

        let kill_status = sigterm(&mut server);
        assert!(
            kill_status.success(),
            "kill -TERM must succeed: {kill_status}"
        );
        let exit = server.child.wait().expect("wait for terminated server");
        assert!(
            exit.success(),
            "SIGTERM must produce a clean exit after flushing acknowledged updates, got {exit}"
        );
        let mut stderr = String::new();
        server
            .child
            .stderr
            .take()
            .expect("capture server stderr")
            .read_to_string(&mut stderr)
            .expect("read structured server logs");
        for evidence in [
            "wayfinder listening",
            "method=POST",
            "uri=/wayfinder/shutdown/update?commitWithin=60000&wt=json&ignored_by_wayfinder=yes",
            "status=200",
            "ignoring unknown request parameter",
            "parameter=ignored_by_wayfinder",
            "received SIGTERM",
            "wayfinder shutdown complete",
        ] {
            assert!(
                stderr.contains(evidence),
                "server logs must contain {evidence:?}, got:\n{stderr}"
            );
        }

        let restarted_addr = unused_localhost_addr();
        let mut restarted = start_server(&schema, &data_dir, restarted_addr);
        wait_until_serving(restarted_addr);
        let (status, response) = http_request(
            restarted_addr,
            "GET",
            "/wayfinder/shutdown/select?q=id:shutdown-doc&fl=id&wt=json",
            "",
        );
        assert_eq!(
            status, 200,
            "restarted server must accept select: {response}"
        );
        let response: Value = serde_json::from_str(&response).expect("select response is JSON");
        assert_eq!(
            response
                .pointer("/response/numFound")
                .and_then(Value::as_u64),
            Some(1),
            "the acknowledged commitWithin=60000 document must be durable after SIGTERM: {response}"
        );

        let kill_status = sigterm(&mut restarted);
        assert!(
            kill_status.success(),
            "kill restarted server: {kill_status}"
        );
        let exit = restarted.child.wait().expect("wait for restarted server");
        assert!(
            exit.success(),
            "restarted server must also exit cleanly: {exit}"
        );
    }
}
