//! Guard: every `finding N` citation in the repo resolves to a finding that
//! actually exists in `docs/solr-ref-findings.md`.
//!
//! This repo's compatibility argument leans on those citations being
//! traceable, and they drift: concurrent branches each claim a number range,
//! a collision forces a renumber (`7a7809e` renumbered 21-24 to 27-30), and
//! nobody sweeps the citations. Issue #198 found a whole band in
//! `tests/mlt.rs` nine low and another in `tests/query_types.rs` fourteen low.
//!
//! ponytail: existence only. Whether finding 63 *supports* the sentence citing
//! it is not checkable here — that stays a human review job. Catching the
//! dangling half is what stops the next renumber from rotting silently.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// The findings doc's own numbering is not gapless — 32, 33, 43, 44, 45, 85
/// and 86 were vacated by renumbers and never reused. A gap is harmless; it
/// only means a citation to it dangles, which is exactly what this test
/// catches. Duplicates are the real hazard, and are rejected outright below.
fn findings_doc() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/solr-ref-findings.md")
}

/// Numbers of the top-level numbered items in the findings doc, with how many
/// times each appears.
fn declared_findings() -> BTreeMap<u32, usize> {
    let text = std::fs::read_to_string(findings_doc()).expect("findings doc is readable");
    let mut counts = BTreeMap::new();
    for line in text.lines() {
        // A finding is `N. **...` at column zero. Nested list items are
        // indented, and `(a)`/`(b)` sub-clauses live inside a finding's prose.
        let Some((num, rest)) = line.split_once(". ") else {
            continue;
        };
        if !num.is_empty() && num.chars().all(|c| c.is_ascii_digit()) && rest.starts_with("**") {
            *counts.entry(num.parse().unwrap()).or_insert(0) += 1;
        }
    }
    assert!(
        counts.len() > 90,
        "parsed only {} findings - the parser stopped matching the doc's shape",
        counts.len()
    );
    counts
}

/// Every citation in `text`, as (number, the snippet it appeared in).
///
/// Handles the forms in use: `finding 12`, `findings 27-30`, `finding 36/37`,
/// `findings 6, 26`, `findings 90, 91 and 92`, `findings 21–25` (en dash),
/// `finding #93`. Newlines are normalised first because citations wrap across
/// lines (`docs/PRD.md:229`).
fn citations(text: &str) -> Vec<(u32, String)> {
    let flat = text.replace(['\n', '\r'], " ");
    let lower = flat.to_ascii_lowercase();
    let mut found = Vec::new();
    let mut at = 0;
    while let Some(hit) = lower[at..].find("finding") {
        let start = at + hit;
        at = start + "finding".len();
        let mut cursor = at;
        if lower[cursor..].starts_with('s') {
            cursor += 1;
        }
        // `docs/solr-ref-findings.md`, `findings from the issue #3 capture`:
        // anything whose next token is not a number is prose, not a citation.
        // The trimmed set covers `finding #93`, `finding-54`, `finding(90)`,
        // `findings_90_and_91`, and a citation that wraps across a comment
        // line (`finding` / `//!   54's`), which newline flattening turns into
        // an embedded comment leader.
        cursor += lower[cursor..].len()
            - lower[cursor..]
                .trim_start_matches([' ', '#', '-', '/', '!', '*', '(', '_'])
                .len();
        let snippet: String = flat[start..].chars().take(60).collect();
        // A citation is a run of numbers joined by separators. `run` is
        // per-citation so a range's low end cannot come from an earlier one.
        let mut run: Vec<u32> = Vec::new();
        loop {
            let rest = &lower[cursor..];
            let digits = rest.len() - rest.trim_start_matches(|c: char| c.is_ascii_digit()).len();
            if digits == 0 {
                break;
            }
            let n: u32 = rest[..digits].parse().unwrap();
            // A range's endpoints imply every finding between them.
            let ranged = lower[..cursor].ends_with('-') || lower[..cursor].ends_with('–');
            if let (true, Some(&prev)) = (ranged, run.last()) {
                run.extend((prev + 1)..n);
            }
            run.push(n);
            cursor += digits;
            let Some(sep) = ["/", ", ", " and ", "-", "–", "#"]
                .into_iter()
                .find(|s| lower[cursor..].starts_with(s))
            else {
                break;
            };
            cursor += sep.len();
        }
        found.extend(run.into_iter().map(|n| (n, snippet.clone())));
        at = cursor.max(at);
    }
    found
}

/// Text files that cite findings. Fixtures and `target/` are excluded.
fn cited_files() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut out = Vec::new();
    let mut stack = vec![root.join("src"), root.join("tests"), root.join("docs")];
    stack.push(root.join("solr-ref/capture.sh"));
    stack.push(root.join("CLAUDE.md"));
    while let Some(path) = stack.pop() {
        if path.is_dir() {
            stack.extend(
                std::fs::read_dir(&path)
                    .unwrap()
                    .map(|e| e.unwrap().path())
                    .filter(|p| p.file_name().unwrap() != "responses"),
            );
        } else if path.is_file() {
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
            if matches!(ext, "rs" | "md" | "sh") {
                out.push(path);
            }
        }
    }
    out
}

/// References to a vacated number that are *deliberately* about the number
/// not existing. `2026-07-28-harness-debt.md` records that 32 and 33 were
/// reserved in the numbering but never used by that work — a true sentence
/// that would otherwise read as two dangling citations.
const INTENTIONAL_VACANT_REFERENCES: &[(&str, u32)] = &[
    ("docs/reports/2026-07-28-harness-debt.md", 32),
    ("docs/reports/2026-07-28-harness-debt.md", 33),
];

#[test]
fn every_citation_resolves_to_a_real_finding() {
    let declared = declared_findings();
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut dangling: Vec<String> = Vec::new();
    for file in cited_files() {
        let text = std::fs::read_to_string(&file).unwrap();
        let rel = file.strip_prefix(root).unwrap().display().to_string();
        for (n, snippet) in citations(&text) {
            if declared.contains_key(&n)
                || INTENTIONAL_VACANT_REFERENCES.contains(&(rel.as_str(), n))
            {
                continue;
            }
            dangling.push(format!("{rel}: finding {n} does not exist -- {snippet:?}"));
        }
    }
    dangling.sort();
    dangling.dedup();
    assert!(
        dangling.is_empty(),
        "{} citations point at findings that are not in docs/solr-ref-findings.md:\n{}",
        dangling.len(),
        dangling.join("\n")
    );
}

#[test]
fn findings_are_numbered_uniquely() {
    let duplicated: BTreeSet<u32> = declared_findings()
        .into_iter()
        .filter(|&(_, count)| count > 1)
        .map(|(n, _)| n)
        .collect();
    assert!(
        duplicated.is_empty(),
        "docs/solr-ref-findings.md numbers {duplicated:?} more than once. A citation of a \
         duplicated number is ambiguous about which finding it means -- give the newer one \
         the next free number and sweep its citations."
    );
}
