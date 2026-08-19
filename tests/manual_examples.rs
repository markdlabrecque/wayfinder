//! The executable quickstart is tested by running its documented shell commands.
//!
//! The Markdown is the sole source for the commands and response expectations:
//! this test extracts its `sh` fences verbatim, so a route, parameter, or
//! assertion can never be silently duplicated here.

#[cfg(unix)]
mod unix {
    use std::net::{SocketAddr, TcpListener};
    use std::process::Command;

    use serde_json::{Value, json};

    const QUICKSTART_MD: &str = include_str!("../manual/getting-started/quickstart.md");
    const CORPUS_JSON: &str = include_str!("../manual/getting-started/corpus.json");

    fn documented_shell_blocks(markdown: &str) -> Vec<&str> {
        let mut remaining = markdown;
        let mut blocks = Vec::new();

        while let Some((_, after_open)) = remaining.split_once("```sh\n") {
            let (block, after_close) = after_open
                .split_once("\n```")
                .expect("every documented sh fence must close");
            blocks.push(block);
            remaining = after_close;
        }

        assert!(
            !blocks.is_empty(),
            "quickstart must provide shell commands to execute"
        );
        blocks
    }

    fn unused_loopback_addr() -> SocketAddr {
        let listener =
            TcpListener::bind("127.0.0.1:0").expect("reserve an ephemeral loopback port");
        let addr = listener
            .local_addr()
            .expect("read reserved loopback address");
        drop(listener);
        addr
    }

    #[test]
    fn documented_quickstart_commands_execute_and_check_their_own_results() {
        let blocks = documented_shell_blocks(QUICKSTART_MD);

        // Every user-visible response must be checked in the same documented
        // command block that produces it. These are ping, update, select,
        // three UI pages, and the post-restart select.
        let response_blocks: Vec<_> = blocks
            .iter()
            .filter(|block| block.contains("response=$(curl"))
            .collect();
        assert_eq!(
            response_blocks.len(),
            7,
            "the quickstart must retain its seven documented response steps"
        );
        for block in response_blocks {
            assert_eq!(
                block.matches("response=$(curl").count(),
                1,
                "each documented response block must capture exactly one curl response: {block}"
            );
            assert!(
                block.contains("assert_response \"$response\""),
                "every documented curl response must be checked in its own command block: {block}"
            );
            assert!(
                block.contains("assert_response \"$response\" 'WAYFINDER_HTTP_STATUS:200'")
                    && block.contains("assert_response \"$response\" 'WAYFINDER_CONTENT_TYPE:"),
                "every documented curl response must check status and content type: {block}"
            );
        }

        let workflow = blocks.join("\n\n");
        assert!(
            workflow.contains("LISTENER=${WAYFINDER_LISTENER:-"),
            "the directly executable quickstart must accept WAYFINDER_LISTENER so its documented loopback workflow can run without claiming a shared port"
        );

        // Keep the canonical fixture validation from the prior executable
        // quickstart test. The command workflow below must index this exact
        // stable corpus, rather than a test-local substitute.
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

        let output = Command::new("sh")
            .args(["-eu", "-c", &workflow])
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .env_remove("WAYFINDER_CONFIG")
            .env("WAYFINDER", env!("CARGO_BIN_EXE_wayfinder"))
            .env("WAYFINDER_LISTENER", unused_loopback_addr().to_string())
            .output()
            .expect("run the documented quickstart shell commands");
        assert!(
            output.status.success(),
            "documented quickstart commands failed (status {}):\nstdout:\n{}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }
}
