//! Expiring guard for what is **still** descoped around `_version_` after
//! #343 — the **write side** (#293, PRD §5 "v3 — `_version_`").
//!
//! Finding 132 (the #307 source sweep) corrected #293's premise. The client
//! reads `_version_` **only** through a JSON facet aggregation
//! (`createJsonFacetAggregation` + `max(_version_)`), on admin "max document
//! version" diagnostics screens — never via `stats.field`, never written,
//! never as an optimistic-concurrency precondition (`versions=true`), never
//! with atomic-update field modifiers. #293 therefore delivered the field
//! (#99/#102: real, fast, populated per document, stats-capable) and rescoped
//! the read path — JSON faceting with aggregations and nesting — to its own
//! item.
//!
//! **That read path landed in #343**, so this file was narrowed to match: it
//! was `version_descope_guard.rs`, it lost `json.facet` from its request
//! needles, and it lost the read-path deferral framing. What it keeps is the
//! coverage #343 did not touch and deleting the file would have silently
//! dropped: the two write-side descopes (`versions=true` optimistic
//! concurrency, atomic-update field modifiers), the trace request-side scan
//! with its positive control, and the finding-132 PRD tripwires.
//!
//! Per CLAUDE.md's rule for deliberate skips, this guard must fail the day the
//! evidence stops holding. When it goes red, the fix is **not** to weaken it
//! — it is to revisit PRD §5's v3 `_version_` decision (#293) with the new
//! evidence in hand.
//!
//! Two evidence channels, mirroring `tests/edismax_descope_guard.rs`:
//!
//!   1. The captured client never sends `_version_` request-side across the 28
//!      committed traces in `solr-ref/search-api/trace/` — not as a doc field
//!      in an update body, not as `versions=true`, not as `max(_version_)`.
//!      (The diagnostics requests that read it are admin screens, not captured
//!      search traffic; #343 serves them, it does not put them in a trace.)
//!   2. The frozen `search_api_solr` 4.4.0 source builds every `_version_`
//!      aggregation through Solarium's JSON-facet API and writes every
//!      document whole through `addDocument(s)`.

use std::path::{Path, PathBuf};

use serde_json::Value;

/// `_version_`-related needles whose request-side presence would mean a client
/// actually exercises a `_version_` feature on a *search* path.
///
/// `json.facet` was deliberately removed from this list when #343 landed:
/// server-side `json.facet` is now implemented, so its appearance in a trace
/// would no longer signal an unmet dependency. `max(_version_)` stays — it is
/// implemented too, but a *search* trace computing it would still overturn
/// "nothing a site searches depends on `_version_`", which is what the
/// write-side descopes below rest on.
const REQUEST_NEEDLES: &[&str] = &["_version_", "versions=true", "max(_version_)"];

const SOURCE: &str = include_str!(
    "../coverage/search_api_solr_4.4.0_source/src/Plugin/search_api/backend/SearchApiSolrBackend.php"
);
const TRACE_DIR: &str = "solr-ref/search-api/trace";
const PRD: &str = include_str!("../docs/PRD.md");

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn trace_files() -> Vec<PathBuf> {
    let dir = root().join(TRACE_DIR);
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
        .map(|entry| entry.expect("dir entry").path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("json"))
        .collect();
    files.sort();
    files
}

fn load(path: &Path) -> Value {
    let raw =
        std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    serde_json::from_str(&raw).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()))
}

// --- trace channel: nothing searched sends `_version_` -------------------

#[test]
fn trace_corpus_is_the_28_traces_the_decision_was_checked_against() {
    let count = trace_files().len();
    assert_eq!(
        count, 28,
        "the `_version_` v1 decision (#293, PRD §5) was checked against exactly 28 committed \
         traces in solr-ref/search-api/trace/; the corpus now has {count}. If new traces were \
         added, re-check every `_version_` needle against their request sides before treating \
         this guard as still valid — see issue #293."
    );
}

/// No captured client request — query string, headers, or body — references
/// `_version_` in any form. This is the premise behind "nothing a site
/// searches depends on `_version_`": the only client path that reads it (the
/// admin diagnostics JSON-facet screen, served since #343) is not captured
/// search traffic, and no client writes it or sends `versions=true`.
#[test]
fn no_trace_request_references_version_in_any_form() {
    for file in trace_files() {
        let capture = load(&file);
        // Serialize the whole request object so the scan covers the path
        // (query string), headers, and a JSON/form body uniformly.
        let request = serde_json::to_string(&capture["request"])
            .unwrap_or_else(|e| panic!("{}: serialize request: {e}", file.display()));
        for needle in REQUEST_NEEDLES {
            assert!(
                !request.contains(needle),
                "{}: the captured request now references `{needle}`. PRD §5's v3 `_version_` \
                 decision (#293) rests on no captured client ever sending `_version_` \
                 request-side — that premise no longer holds and the decision must be revisited, \
                 not silently kept.",
                file.display()
            );
        }
    }
}

/// Positive control for the request-side scan: `_version_` IS present in
/// captured *responses* (Solr returns it on every document), so the scan's
/// absence of `_version_` on the request side is a real asymmetry, not a
/// corpus where `_version_` never appears at all. Without this, the
/// request-side guard could stay permanently, falsely green if `_version_`
/// were stripped from responses upstream of capture.
#[test]
fn version_is_present_in_trace_responses_so_the_request_scan_is_not_blind() {
    let mut seen = false;
    for file in trace_files() {
        let capture = load(&file);
        if serde_json::to_string(&capture["response"])
            .unwrap_or_default()
            .contains("_version_")
        {
            seen = true;
            break;
        }
    }
    assert!(
        seen,
        "no captured response contains `_version_`. The request-side guard's claim that no \
         request sends `_version_` is only meaningful while responses still carry it (Solr \
         returns it on every doc); pick a new positive control or revisit the guard."
    );
}

// --- source channel: finding 132, made executable ------------------------
//
// The diagnostics JSON-facet requests are not in the 28 search traces, so the
// source is the only frozen evidence for HOW the client uses `_version_`. These
// assertions re-run that sweep, and become meaningful again the day the
// coverage source is upgraded to a new `search_api_solr` version.

/// `_version_` is read only through a JSON facet aggregation. Both tokens
/// must appear together: `createJsonFacetAggregation` is Solarium's JSON-facet
/// API and `max(_version_)` is the aggregate the diagnostics screens compute.
///
/// Since #343 this no longer guards a deferral — it pins the *reason* #343 was
/// built and the shape it was built to. If the client's read path moves off
/// Solarium's JSON-facet API, `src/json_facet.rs`'s scope (bare-string
/// aggregation, `type: terms` nesting, `max()` only — spec §1a, finding 167)
/// is aimed at a client that no longer exists and needs re-deriving, not
/// extending.
#[test]
fn source_reads_version_only_through_a_json_facet_aggregation() {
    assert!(
        SOURCE.contains("createJsonFacetAggregation"),
        "the source no longer builds a JSON facet aggregation. PRD §5's v3 `_version_` decision \
         (#293) records that the client reads `_version_` via `createJsonFacetAggregation` \
         (finding 132), and #343 built `json.facet` to exactly that shape; if that API usage \
         moved, re-derive the wire form from the new source before extending src/json_facet.rs."
    );
    assert!(
        SOURCE.contains("max(_version_)"),
        "the source no longer aggregates `max(_version_)`. The diagnostics `maxVersion` probe \
         (finding 132) is the client's only `_version_` read; its absence means the premise moved."
    );
}

/// `_version_` is never sent as an optimistic-concurrency precondition. Solarium
/// would surface this as a `versions` update option; the source has none.
#[test]
fn source_never_requests_versions_true_for_optimistic_concurrency() {
    assert!(
        !SOURCE.contains("versions=true"),
        "the source now sends `versions=true`. PRD §5's v3 `_version_` decision (#293) descoped \
         optimistic concurrency (`versions=true` + 409-on-stale) specifically because no client \
         sends it — that premise no longer holds and the descope must be revisited."
    );
}

/// Every write goes through whole-document `addDocument(s)`. Solarium's
/// atomic-update API (`set`/`inc`/`add`/`add-distinct`/`remove`) is a different
/// call path the source does not use; asserting the whole-doc path is present
/// pins the write mechanism the descope's "no atomic updates" clause rests on.
///
/// ponytail: this asserts the *positive* write mechanism rather than proving
/// the negative "no atomic modifier anywhere in arbitrary PHP". The trace
/// request-side guard above is the backstop for an atomic-update body, since
/// any such body would carry a `_version_`-independent `{\"set\":...}` shape
/// that this source scan does not attempt to detect.
#[test]
fn source_writes_whole_documents_not_atomic_updates() {
    assert!(
        SOURCE.contains("addDocuments") || SOURCE.contains("addDocument"),
        "the source no longer writes through Solarium's whole-document `addDocument(s)`. PRD \
         §5's v3 `_version_` decision (#293) descoped atomic-update field modifiers because the \
         client writes whole documents only; if the write path changed, re-check whether atomic \
         updates entered the picture before trusting this guard."
    );
}

// --- PRD correction (#293's documented defect) ---------------------------
//
// The PRD's v3 `_version_` section previously stated the client reads
// `_version_` via `stats.field=_version_&function=max(_version_)`. Finding 132
// corrected that to JSON faceting. These are tripwires against the wrong
// premise being restored, scoped to one section so a coincidental token
// elsewhere in the PRD cannot satisfy them.

/// The whole section from its heading up to (but not including) the next `---`
/// separator or `## ` heading.
fn version_section() -> &'static str {
    let start = PRD
        .find("### v3 — `_version_`")
        .expect("PRD must still contain the v3 — `_version_` section");
    let rest = &PRD[start..];
    // The section ends at the `---` rule that precedes `## 6. Tuning knobs`.
    let end = rest
        .find("\n---\n")
        .or_else(|| rest[1..].find("\n## ").map(|i| i + 1))
        .unwrap_or(rest.len());
    &rest[..end]
}

fn version_paragraphs() -> Vec<&'static str> {
    version_section()
        .split("\n\n")
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect()
}

#[test]
fn prd_version_section_records_the_json_facet_premise() {
    // One block must tie `_version_`'s client usage to JSON faceting. Checked
    // block-local (not section-wide) so an unrelated `json`/`facet` mention
    // cannot satisfy it.
    assert!(
        version_paragraphs()
            .iter()
            .map(|p| p.to_lowercase())
            .any(|p| p.contains("json facet") || p.contains("json.facet")),
        "PRD §5's v3 `_version_` section should record finding 132's correction: the client \
         reads `_version_` through a JSON facet aggregation, not `stats.field`. No single block \
         ties `_version_` to JSON faceting."
    );
}

#[test]
fn prd_version_section_no_longer_attributes_stats_field_to_the_client() {
    // The specific wrong claim was that the client references `_version_`
    // "via `stats.field=_version_`" as its read path. `stats.field` may still
    // appear in the section as a v1 capability of the field (it is statable),
    // so this targets the *attribution to the client*, not the bare token.
    for p in version_paragraphs() {
        let lower = p.to_lowercase();
        let attributes_to_client =
            lower.contains("client") || lower.contains("references") || lower.contains("reads");
        if lower.contains("stats.field") && attributes_to_client {
            panic!(
                "PRD §5's v3 `_version_` section still attributes `stats.field` to the client in \
                 this block:\n{p}\nFinding 132 corrected this: the client reads `_version_` via \
                 JSON faceting, never `stats.field`. Update the section (issue #293)."
            );
        }
    }
}

#[test]
fn prd_version_section_references_the_decision_issue() {
    assert!(
        version_section().contains("#293"),
        "PRD §5's v3 `_version_` section should reference issue #293 so a future reader can find \
         the decision and its evidence (finding 132)."
    );
}

/// The mirror image of the guard this file used to be: #343 landed the read path, so the PRD must
/// stop calling it deferred. Without this, the narrowing done in #343 could silently rot back into
/// a doc that describes a descope Wayfinder no longer has — the same failure mode CLAUDE.md's
/// expiring-skip rule exists to prevent, pointed the other way.
#[test]
fn prd_version_section_records_the_json_facet_read_path_as_landed_not_deferred() {
    let section = version_section();
    assert!(
        section.contains("#343"),
        "PRD §5's v3 `_version_` section does not reference #343. The JSON-facet read path it \
         once deferred has landed; the section must say so and point at the issue, or a reader \
         will re-descope a shipped feature."
    );
    for p in version_paragraphs() {
        let lower = p.to_lowercase();
        let names_the_read_path = lower.contains("json facet") || lower.contains("json.facet");
        let calls_it_deferred = lower.contains("deferred") || lower.contains("not v1 work");
        assert!(
            !(names_the_read_path && calls_it_deferred),
            "PRD §5's v3 `_version_` section still describes the JSON-facet read path as \
             deferred in this block:\n{p}\nIt shipped in #343. Update the wording rather than \
             relaxing this assertion."
        );
    }
}

/// The §5 parity table used to claim "`json.facet` appears nowhere in its source". That is
/// literally true (the PHP uses Solarium's `createJsonFacetAggregation`, not the bare string)
/// but functionally false and directly contradicts finding 132, which this decision rests on.
/// Trip the moment that misleading wording returns anywhere in the PRD.
#[test]
fn prd_does_not_claim_json_facet_is_absent_from_the_source() {
    assert!(
        !PRD.contains("json.facet` appears nowhere"),
        "PRD claims `json.facet` appears nowhere in the client source. Finding 132 shows the \
         opposite: the client reads `_version_` through Solarium's JSON Facet API \
         (`createJsonFacetAggregation` + `max(_version_)`). Correct the wording."
    );
}
