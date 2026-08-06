//! Release-artifact contract for issue #412.
//!
//! These tests inspect the repository's two published-artifact boundaries without
//! requiring Docker or GitHub Actions to run during the hermetic Rust test suite.

use std::fs;
use std::path::{Path, PathBuf};

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn uncommented_lines(contents: &str) -> Vec<String> {
    contents
        .lines()
        .map(|line| {
            line.split_once('#')
                .map_or(line, |(before, _)| before)
                .trim()
        })
        .filter(|line| !line.is_empty())
        .map(str::to_ascii_lowercase)
        .collect()
}

fn final_docker_stage(dockerfile: &str) -> String {
    let stage_starts: Vec<_> = dockerfile
        .lines()
        .enumerate()
        .filter_map(|(index, line)| {
            line.trim_start()
                .to_ascii_lowercase()
                .starts_with("from ")
                .then_some(index)
        })
        .collect();
    let start = *stage_starts
        .last()
        .expect("Dockerfile must have a FROM stage");
    let end = dockerfile.lines().count();
    dockerfile
        .lines()
        .skip(start)
        .take(end - start)
        .collect::<Vec<_>>()
        .join("\n")
}

fn copy_instruction_targets_presets(stage: &str) -> bool {
    let mut instruction = String::new();
    for line in stage.lines() {
        let line = line
            .split_once('#')
            .map_or(line, |(before, _)| before)
            .trim();
        if line.is_empty() {
            continue;
        }
        instruction.push_str(line.trim_end_matches('\\'));
        if line.ends_with('\\') {
            instruction.push(' ');
            continue;
        }

        let mut words = instruction.split_whitespace();
        let is_copy = words
            .next()
            .is_some_and(|word| word.eq_ignore_ascii_case("copy"));
        if is_copy {
            let normalized = instruction.replace(['[', ']', '"', ','], " ");
            let arguments: Vec<_> = normalized
                .split_whitespace()
                .filter(|word| !word.starts_with("--"))
                .collect();
            if arguments.last().is_some_and(|target| *target == "/presets") {
                return true;
            }
        }
        instruction.clear();
    }
    false
}

fn release_triggered(workflow: &str) -> bool {
    let mut in_on_block = false;
    for raw_line in workflow.lines() {
        let without_comment = raw_line
            .split_once('#')
            .map_or(raw_line, |(before, _)| before);
        let indent = without_comment.len() - without_comment.trim_start().len();
        let line = without_comment.trim().to_ascii_lowercase();
        if line.is_empty() {
            continue;
        }
        if indent == 0 {
            in_on_block = line == "on:";
            if line.starts_with("on: [") && line.contains("release") {
                return true;
            }
            if line == "on: release" {
                return true;
            }
        } else if in_on_block && line.starts_with("release:") {
            return true;
        }
    }
    false
}

fn release_workflows() -> Vec<(PathBuf, String)> {
    let workflows = root().join(".github/workflows");
    fs::read_dir(&workflows)
        .unwrap_or_else(|error| panic!("read {}: {error}", workflows.display()))
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            matches!(
                path.extension().and_then(|ext| ext.to_str()),
                Some("yml" | "yaml")
            )
        })
        .filter_map(|path| {
            let contents = fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
            release_triggered(&contents).then_some((path, contents))
        })
        .collect()
}

fn matrix_entry_has_native_runner(lines: &[String], platform: &str, runner: &str) -> bool {
    lines.iter().enumerate().any(|(start, line)| {
        if !line.contains("platform:") || !line.contains(platform) {
            return false;
        }
        lines[start..]
            .iter()
            .skip(1)
            .take_while(|next| !next.starts_with("- "))
            .any(|next| next.contains("runner:") && next.contains(runner))
    })
}

#[test]
fn final_scratch_image_includes_presets_at_root() {
    let dockerfile = fs::read_to_string(root().join("Dockerfile")).expect("read Dockerfile");
    let stage = final_docker_stage(&dockerfile);
    let base = stage
        .lines()
        .next()
        .expect("final Docker stage must have a FROM instruction")
        .split_whitespace()
        .nth(1);

    assert_eq!(
        base,
        Some("scratch"),
        "the final Docker stage must be FROM scratch"
    );
    assert!(
        copy_instruction_targets_presets(&stage),
        "the final scratch stage must COPY presets to /presets"
    );
}

#[test]
fn release_publish_workflow_builds_and_merges_native_multiarch_ghcr_image() {
    let release_workflows = release_workflows();
    assert!(
        !release_workflows.is_empty(),
        "a publish workflow must trigger on GitHub releases"
    );

    let (path, workflow) = release_workflows
        .into_iter()
        .find(|(_, workflow)| {
            uncommented_lines(workflow)
                .iter()
                .any(|line| line.contains("ghcr.io/") && line.contains("wayfinder"))
        })
        .expect("a release-triggered publish workflow must target ghcr.io/${owner}/wayfinder");
    let lines = uncommented_lines(&workflow);

    assert!(
        lines.iter().any(|line| line == "packages: write"),
        "{} must grant packages: write",
        path.display()
    );
    assert!(
        matrix_entry_has_native_runner(&lines, "linux/amd64", "ubuntu-latest"),
        "{} must include a linux/amd64 matrix entry on ubuntu-latest",
        path.display()
    );
    assert!(
        matrix_entry_has_native_runner(&lines, "linux/arm64", "ubuntu-24.04-arm"),
        "{} must include a linux/arm64 matrix entry on ubuntu-24.04-arm",
        path.display()
    );
    assert!(
        lines
            .iter()
            .any(|line| line.starts_with("runs-on:") && line.contains("matrix.")),
        "{} must run the architecture matrix on its selected native runner",
        path.display()
    );
    assert!(
        !workflow.to_ascii_lowercase().contains("setup-qemu-action"),
        "{} must not emulate either architecture with QEMU",
        path.display()
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("push-by-digest=true")),
        "{} must push each architecture by digest",
        path.display()
    );
    assert!(
        lines.iter().any(|line| line.contains("imagetools create")),
        "{} must merge the per-architecture digests with buildx imagetools create",
        path.display()
    );
}
