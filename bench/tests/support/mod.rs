//! Issue #62: shared test-seam plumbing for exercising pieces of
//! `bench/run.sh` in isolation -- no Docker, no network, hermetic.
//!
//! `run.sh` is a monolithic `set -euo pipefail` script with side effects on
//! nearly every line (docker, curl, cargo build...), so we can't just
//! `source` the whole thing. Instead these helpers pull a single named shell
//! function's source text out of the script (by brace-matching from its
//! `name() {` line) and run it in a throwaway bash process, with a stub
//! `docker`/`curl` on `PATH` standing in for the real tools.
//!
//! If a named function doesn't exist yet in `run.sh`, extraction returns
//! `None` -- that's the expected "missing behavior" signal for tests that
//! pin down a seam the implementor still needs to add (see
//! `run_sh_schema_check.rs`).

// Shared across several test binaries; not every binary uses every helper
// (mirrors the `tests/common/mod.rs` convention at the repo root -- keep
// this the one place the allow lives, don't add a second on `mod support;`).
#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Absolute path to `bench/run.sh` under test.
pub fn run_sh_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("run.sh")
}

pub fn run_sh_source() -> String {
    fs::read_to_string(run_sh_path())
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", run_sh_path().display()))
}

/// Pull the source text of a top-level `name() { ... }` shell function out
/// of `source`, matching braces to find the end. Returns `None` if no such
/// function is defined -- this is the expected result for a seam function
/// the implementor hasn't added yet, and callers should treat that as a
/// clear "missing behavior" test failure rather than panicking blindly.
pub fn extract_bash_function(source: &str, name: &str) -> Option<String> {
    let lines: Vec<&str> = source.lines().collect();
    let start_idx = lines
        .iter()
        .position(|l| l.trim_start().starts_with(&format!("{name}()")))?;

    let mut depth = 0i32;
    let mut end_idx = None;
    for (i, line) in lines.iter().enumerate().skip(start_idx) {
        for ch in line.chars() {
            match ch {
                '{' => depth += 1,
                '}' => depth -= 1,
                _ => {}
            }
        }
        if depth == 0 {
            end_idx = Some(i);
            break;
        }
    }
    let end_idx = end_idx?;
    Some(lines[start_idx..=end_idx].join("\n"))
}

/// Returns `true` and the function name if `line` (verbatim, no
/// leading-whitespace stripping applied by the caller -- top-level
/// functions in `run.sh` are unindented) is a top-level `name() {`
/// definition line, optionally followed by a trailing `# comment`.
fn top_level_function_name(line: &str) -> Option<&str> {
    if line != line.trim_start() {
        return None;
    }
    let idx = line.find("()")?;
    let name = &line[..idx];
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return None;
    }
    let after = line[idx + 2..].trim_start();
    if after.starts_with('{') {
        Some(name)
    } else {
        None
    }
}

/// Returns `source` with every top-level `name() { ... }` function body
/// (definition line through its matching closing brace, inclusive) blanked
/// out to empty lines, line numbers preserved.
///
/// Issue #251 round 2 (reviewer finding): a source-order guard that scans
/// for "any line containing the function name" cannot tell a real call
/// site from a mention of the name inside that function's OWN body (an
/// error message naming itself, a comment referencing a sibling function).
/// Stripping bodies first means every remaining match is a genuine call
/// site or a genuine standalone occurrence (like a literal URL fragment),
/// never text quoted inside the function's own definition.
pub fn strip_function_bodies(source: &str) -> String {
    let lines: Vec<&str> = source.lines().collect();
    let mut out: Vec<String> = lines.iter().map(|s| s.to_string()).collect();
    let mut i = 0;
    while i < lines.len() {
        if let Some(_name) = top_level_function_name(lines[i]) {
            let mut depth = 0i32;
            for ch in lines[i].chars() {
                match ch {
                    '{' => depth += 1,
                    '}' => depth -= 1,
                    _ => {}
                }
            }
            let mut end = i;
            let mut j = i + 1;
            while depth != 0 && j < lines.len() {
                for ch in lines[j].chars() {
                    match ch {
                        '{' => depth += 1,
                        '}' => depth -= 1,
                        _ => {}
                    }
                }
                end = j;
                if depth == 0 {
                    break;
                }
                j += 1;
            }
            for line in out.iter_mut().take(end + 1).skip(i) {
                line.clear();
            }
            i = end + 1;
            continue;
        }
        i += 1;
    }
    out.join("\n")
}

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A fresh scratch directory under the OS temp dir, unique per call.
pub fn fresh_scratch_dir(label: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock should be after the epoch")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "wayfinder-bench-test-{label}-{}-{nanos}-{n}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

/// Write `contents` as an executable file at `dir/name` (used for stub
/// `docker`/`curl` binaries placed ahead of the real ones on `PATH`).
pub fn write_executable(dir: &Path, name: &str, contents: &str) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, contents).expect("write stub executable");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("chmod stub");
    }
    path
}

/// Run `script_body` under `bash -euo pipefail`, with `stub_bin_dir`
/// prepended to `PATH` so stub tools shadow the real ones, and the given
/// extra environment variables set.
pub fn run_bash(script_body: &str, stub_bin_dir: &Path, extra_env: &[(&str, &str)]) -> Output {
    let real_path = std::env::var("PATH").unwrap_or_default();
    let path = format!("{}:{real_path}", stub_bin_dir.display());

    let full_script = format!("set -euo pipefail\n{script_body}\n");

    let mut cmd = Command::new("bash");
    cmd.arg("-c").arg(full_script).env("PATH", path);
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    cmd.output().expect("failed to spawn bash")
}
