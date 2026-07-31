//! Server-rendered admin UI (PRD §5, "v2.5 — Admin web UI"; issue #94).
//!
//! Tracer bullet scope: one page, rendered by `askama` at `GET /ui`, showing
//! the single core this process serves — its name, its live document count,
//! and its on-disk size. Values come from the same in-process `CoreIndex` the
//! query pipeline reads from; there is no stats subsystem and no core
//! registry behind this (Wayfinder is single-core-per-process — see this
//! crate's module doc).
//!
//! The page is read-only: rendering it takes a searcher and stats a
//! directory, and commits/reloads nothing.
//!
//! Issue #127 adds the second page, `GET /ui/query`: a form over the core's
//! own `/select`, rendering that endpoint's real JSON response. This module
//! only *renders* it — the query is executed by `crate::select` itself (see
//! `crate::query_ui`), so there is no second query-parsing/execution path to
//! keep in sync with the wire API.
//!
//! Issue #128 adds the third page, `GET /ui/schema`: a read-only view of the
//! core's fields, dynamic-field rules, and copy-field pairs, rendered from the
//! in-process `WayfinderSchema` the core was opened with. As with the query
//! tester, there is no second parsing path — the page shows what the running
//! index is actually using, not what the TOML on disk says now.
//!
//! Issue #129 adds the fourth page, `GET /ui/stats`: doc count, segment
//! count, on-disk size and process uptime, again read straight off the live
//! `CoreIndex` and the process's own start instant. It reports no resident
//! memory: Wayfinder is mmap-based, so there is no JVM-heap-shaped figure to
//! show, and the page says so in prose (PRD §5 v2.5, restating §6's
//! absent-heap-knob honesty) instead of fabricating one.

use crate::schema::{CopyFieldConfig, DynamicFieldConfig, FieldConfig};
use askama::Template;
use serde_json::Value;
use std::time::Duration;

/// Bytes per unit step. Binary (1024), labelled with the SI-ish short names
/// Solr's own admin UI uses, which is the convention operators expect.
const STEP: u64 = 1024;

#[derive(Template)]
#[template(path = "core.html")]
struct CorePage<'a> {
    core_name: &'a str,
    doc_count: u64,
    size_bytes: u64,
    size_human: String,
}

/// Renders the core page to HTML.
///
/// `size_bytes` is rendered both exactly and in a human-readable form: the
/// exact figure is what an operator diffing two deployments needs, the
/// rounded one is what makes the page readable.
pub fn render_core_page(
    core_name: &str,
    doc_count: u64,
    size_bytes: u64,
) -> Result<String, askama::Error> {
    CorePage {
        core_name,
        doc_count,
        size_bytes,
        size_human: human_size(size_bytes),
    }
    .render()
}

/// `1536` -> `"1.5 KB"`, `0` -> `"0 B"`.
///
/// ponytail: one decimal place, binary steps, no locale awareness. Ceiling:
/// purely cosmetic — the exact byte count is rendered alongside it, so
/// nothing downstream parses this string.
fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= STEP as f64 && unit < UNITS.len() - 1 {
        value /= STEP as f64;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} {}", UNITS[0])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// The query tester's form state, so the page can be re-rendered with what
/// the operator typed (including after a 400 — an error is not a dead end).
///
/// Borrowed, not owned: every value here is a slice of the request's already
/// parsed `Params`.
pub struct QueryForm<'a> {
    pub q: &'a str,
    pub fq: &'a str,
    pub fl: &'a str,
    pub rows: &'a str,
    pub start: &'a str,
    pub facet_field: &'a str,
    pub facet: bool,
}

#[derive(Template)]
#[template(path = "query.html")]
struct QueryPage<'a> {
    core_name: &'a str,
    q: &'a str,
    fq: &'a str,
    fl: &'a str,
    rows: &'a str,
    start: &'a str,
    facet_field: &'a str,
    facet: bool,
    /// False on first load: the form renders, but no `/select` response
    /// block does, because no query has been run.
    has_result: bool,
    result_status: u16,
    /// Pre-rendered, HTML-safe JSON (see `render_query_page`); emitted with
    /// `|safe` because HTML-escaping it would corrupt the JSON text itself.
    result_json: String,
}

/// Renders the query tester page.
///
/// `result` is `None` for the first/empty load and `Some((status, body))`
/// after a submission, where both come straight from `crate::select` — the
/// same handler `/solr/{core}/select` routes to. The body is pretty-printed
/// and safe-escaped for the HTML context, and nothing else: no field is
/// dropped, renamed, or summarised. What the page shows is the wire
/// response, `responseHeader.QTime` and all. Normalising the *rendered*
/// output — even of a field as inert as this crate's hardcoded `QTime` — is
/// exactly the "widen the normaliser to hide a divergence" move the
/// compatibility contract forbids; a test that needs to ignore a variable
/// field normalises its own comparison instead.
pub fn render_query_page(
    core_name: &str,
    form: &QueryForm<'_>,
    result: Option<(u16, &str)>,
) -> Result<String, askama::Error> {
    let (has_result, result_status, result_json) = match result {
        None => (false, 0, String::new()),
        Some((status, body)) => (true, status, html_safe_json(body)),
    };
    QueryPage {
        core_name,
        q: form.q,
        fq: form.fq,
        fl: form.fl,
        rows: form.rows,
        start: form.start,
        facet_field: form.facet_field,
        facet: form.facet,
        has_result,
        result_status,
        result_json,
    }
    .render()
}

#[derive(Template)]
#[template(path = "schema.html")]
struct SchemaPage<'a> {
    core_name: &'a str,
    fields: &'a [FieldConfig],
    dynamic_fields: &'a [DynamicFieldConfig],
    copy_fields: &'a [CopyFieldConfig],
}

/// Renders the schema view page.
///
/// Every value comes from the `WayfinderSchema` the core already holds in
/// memory (loaded once by `CoreIndex::open`) — the schema TOML is never read
/// again to serve this page, so the view cannot drift from the schema the
/// running index actually uses, and cannot fail on a file that moved after
/// startup.
///
/// Read-only, like the core page: nothing here takes a searcher, a writer, or
/// any request input.
pub fn render_schema_page(
    core_name: &str,
    fields: &[FieldConfig],
    dynamic_fields: &[DynamicFieldConfig],
    copy_fields: &[CopyFieldConfig],
) -> Result<String, askama::Error> {
    SchemaPage {
        core_name,
        fields,
        dynamic_fields,
        copy_fields,
    }
    .render()
}

#[derive(Template)]
#[template(path = "stats.html")]
struct StatsPage<'a> {
    core_name: &'a str,
    doc_count: u64,
    segment_count: usize,
    size_bytes: u64,
    size_human: String,
    uptime_secs: u64,
    uptime_human: String,
}

/// Renders the index-stats page.
///
/// Every figure is derived at request time from the same in-process state the
/// query pipeline uses — the live searcher (doc count, segment count), a walk
/// of the core's data dir (size), and the process's own start instant
/// (uptime). There is no stats-collection subsystem behind this, and nothing
/// here is cached, so the page cannot report a stale figure.
///
/// Deliberately absent: any resident-memory figure. Tantivy is mmap-based and
/// the OS page cache does the work a JVM heap does, so there is no
/// heap-shaped value to report; PRD §5 v2.5 (and §6's absent-heap-knob
/// precedent) call for saying so in prose rather than fabricating one, which
/// is what `templates/stats.html` does.
///
/// Read-only, like the core and schema pages: no form, no params, no
/// mutation.
pub fn render_stats_page(
    core_name: &str,
    doc_count: u64,
    segment_count: usize,
    size_bytes: u64,
    uptime: Duration,
) -> Result<String, askama::Error> {
    let uptime_secs = uptime.as_secs();
    StatsPage {
        core_name,
        doc_count,
        segment_count,
        size_bytes,
        size_human: human_size(size_bytes),
        uptime_secs,
        uptime_human: human_duration(uptime_secs),
    }
    .render()
}

/// `3723` -> `"1h 2m 3s"`, `0` -> `"0s"`.
///
/// ponytail: whole seconds, largest-unit-first, no locale awareness and no
/// units above days. Ceiling: purely cosmetic — the exact second count is
/// rendered alongside it (the same exact-plus-readable pairing
/// `templates/core.html` uses for size), so nothing parses this string. It is
/// rendered *after* the exact figure rather than before it so the first number
/// on the row is always the monotonically increasing one.
fn human_duration(total_secs: u64) -> String {
    let days = total_secs / 86_400;
    let hours = (total_secs % 86_400) / 3_600;
    let minutes = (total_secs % 3_600) / 60;
    let seconds = total_secs % 60;
    let mut parts = Vec::new();
    if days > 0 {
        parts.push(format!("{days}d"));
    }
    if days > 0 || hours > 0 {
        parts.push(format!("{hours}h"));
    }
    if days > 0 || hours > 0 || minutes > 0 {
        parts.push(format!("{minutes}m"));
    }
    parts.push(format!("{seconds}s"));
    parts.join(" ")
}

/// Pretty-prints a `/select` response body and makes it safe to emit into
/// HTML *without* HTML-escaping it.
///
/// Pretty-printing and escaping are the only transformations: the JSON that
/// reaches the page parses back to a value identical to `/select`'s own
/// body, field for field.
///
/// HTML-escaping is not an option here: `&quot;` in place of `"` would stop
/// the rendered text from being the JSON it claims to be. Instead the three
/// characters that can escape a `<pre>` context — `<`, `>`, `&` — are
/// replaced with their `\uXXXX` JSON escapes, which are *legal JSON* that
/// parses back to the identical value, so the page cannot be used to inject
/// markup via indexed document content.
///
/// A body that is not valid JSON (which `/select` never produces) is passed
/// through with the same three replacements, so the escaping guarantee holds
/// on every path.
fn html_safe_json(body: &str) -> String {
    let text = match serde_json::from_str::<Value>(body) {
        Ok(v) => serde_json::to_string_pretty(&v).unwrap_or_else(|_| body.to_string()),
        Err(_) => body.to_string(),
    };
    // Only ever appear inside JSON string literals, so replacing them
    // wholesale cannot corrupt the structure.
    text.replace('&', "\\u0026")
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blank_form() -> QueryForm<'static> {
        QueryForm {
            q: "",
            fq: "",
            fl: "",
            rows: "",
            start: "",
            facet_field: "",
            facet: false,
        }
    }

    #[test]
    fn human_size_uses_bytes_below_a_kilobyte() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(1023), "1023 B");
    }

    #[test]
    fn human_size_steps_up_by_1024() {
        assert_eq!(human_size(1024), "1.0 KB");
        assert_eq!(human_size(1536), "1.5 KB");
        assert_eq!(human_size(5 * 1024 * 1024), "5.0 MB");
    }

    #[test]
    fn core_page_renders_name_count_and_size() {
        let html = render_core_page("content", 5, 4096).expect("template must render");
        assert!(html.contains("content"));
        assert!(html.contains("5"));
        assert!(html.contains("4.0 KB"));
        assert!(html.contains("4096 bytes"));
    }

    #[test]
    fn core_page_escapes_the_core_name() {
        let html = render_core_page("<script>", 0, 0).expect("template must render");
        assert!(!html.contains("<script>"));
        assert!(html.contains("&#60;script&#62;"));
    }

    #[test]
    fn query_page_first_load_has_the_form_and_no_result_block() {
        let html = render_query_page("content", &blank_form(), None).expect("template must render");
        for param in ["q", "fq", "fl", "rows", "start", "facet.field"] {
            assert!(
                html.contains(&format!("name=\"{param}\"")),
                "missing form field {param}"
            );
        }
        assert!(
            !html.contains("<pre>"),
            "no response block before a query runs"
        );
    }

    #[test]
    fn query_page_echoes_the_submitted_values_back_into_the_form() {
        let form = QueryForm {
            q: "quick",
            fq: "category:classic",
            fl: "id",
            rows: "5",
            start: "1",
            facet_field: "category",
            facet: true,
        };
        let html = render_query_page("content", &form, None).expect("template must render");
        assert!(html.contains("name=\"q\" value=\"quick\""));
        assert!(html.contains("name=\"fq\" value=\"category:classic\""));
        assert!(html.contains("name=\"facet.field\" value=\"category\""));
        assert!(html.contains("checked"));
    }

    #[test]
    fn query_page_renders_the_status_and_the_json_body() {
        let html = render_query_page(
            "content",
            &blank_form(),
            Some((400, r#"{"error":{"msg":"nope","code":400}}"#)),
        )
        .expect("template must render");
        assert!(html.contains("400"));
        assert!(html.contains("\"msg\": \"nope\""));
    }

    /// The page renders the wire response, not a normalised view of it: no
    /// field is dropped on the way out, `QTime` and `_version_` included.
    /// Guards against reintroducing the impl-side normalisation review round
    /// 1 flagged (a divergence, and an incomplete one — it stripped `QTime`
    /// but not `_version_`/`_root_`).
    #[test]
    fn html_safe_json_drops_nothing_from_the_envelope() {
        let body = r#"{"responseHeader":{"status":0,"QTime":7,"params":{"q":"a"}},"response":{"numFound":1,"docs":[{"id":"doc1","_version_":123,"_root_":"doc1"}]}}"#;
        let out = html_safe_json(body);
        let parsed: Value = serde_json::from_str(&out).expect("rendered output is still JSON");
        let original: Value = serde_json::from_str(body).expect("fixture parses");
        assert_eq!(
            parsed, original,
            "rendering must be pretty-print + escape only, never a field filter"
        );
    }

    #[test]
    fn html_safe_json_neutralises_markup_without_changing_the_parsed_value() {
        // Breakout-shaped: the JSON renders inside a `<pre>`, so the vector
        // that matters is closing that element and opening a script.
        let payload = "</pre><script>alert(1)</script>&amp;";
        let body = serde_json::json!({"response": {"docs": [{"body": payload}]}}).to_string();
        let out = html_safe_json(&body);
        assert!(!out.contains('<'), "no raw `<` may reach the page: {out}");
        assert!(!out.contains('>'), "no raw `>` may reach the page: {out}");
        assert!(!out.contains('&'), "no raw `&` may reach the page: {out}");
        assert!(
            !out.contains("</pre"),
            "the `<pre>` context must not be escapable: {out}"
        );
        let parsed: Value = serde_json::from_str(&out).expect("escaped output is still JSON");
        assert_eq!(
            parsed["response"]["docs"][0]["body"], payload,
            "escaping must not change the value the JSON parses back to"
        );
    }

    /// The form echo is an *attribute* context, not the text context
    /// `core_page_escapes_the_core_name` covers: a `"` that survives into
    /// `value="..."` closes the attribute and lets the rest of the value
    /// become markup (reflected XSS via a crafted link to the tester).
    #[test]
    fn query_page_escapes_submitted_values_in_the_form_attribute_context() {
        let payload = "\" onfocus=\"alert(1)";
        let form = QueryForm {
            q: payload,
            fq: payload,
            fl: payload,
            rows: payload,
            start: payload,
            facet_field: payload,
            facet: false,
        };
        let html = render_query_page("content", &form, None).expect("template must render");
        assert!(
            !html.contains("value=\"\" onfocus="),
            "a submitted value must never break out of its attribute: {html}"
        );
        assert!(
            !html.contains("onfocus=\"alert(1)\""),
            "no attacker-controlled event handler may be emitted: {html}"
        );
        assert!(
            html.contains("&#34; onfocus=&#34;alert(1)"),
            "the payload must appear escaped, not dropped: {html}"
        );
    }

    #[test]
    fn html_safe_json_passes_through_a_non_json_body_escaped() {
        let out = html_safe_json("<b>not json</b>");
        assert_eq!(out, "\\u003cb\\u003enot json\\u003c/b\\u003e");
    }

    #[test]
    fn human_duration_counts_up_from_seconds() {
        assert_eq!(human_duration(0), "0s");
        assert_eq!(human_duration(59), "59s");
        assert_eq!(human_duration(60), "1m 0s");
        assert_eq!(human_duration(3_723), "1h 2m 3s");
        assert_eq!(human_duration(90_061), "1d 1h 1m 1s");
    }

    #[test]
    fn stats_page_renders_every_figure_it_is_given() {
        let html = render_stats_page("content", 5, 3, 4096, Duration::from_secs(3_723))
            .expect("template must render");
        assert!(html.contains("content"));
        assert!(html.contains("<td>5</td>"), "doc count: {html}");
        assert!(html.contains("<td>3</td>"), "segment count: {html}");
        assert!(html.contains("4.0 KB (4096 bytes)"), "size: {html}");
        assert!(html.contains("3723 seconds (1h 2m 3s)"), "uptime: {html}");
    }

    /// The honesty line, pinned: prose about mmap and no fabricated figure.
    /// Guards the PRD §5 v2.5 / §6 requirement against a later edit that
    /// "helpfully" adds a resident-memory number to the page.
    #[test]
    fn stats_page_reports_no_resident_memory_figure() {
        let html = render_stats_page("content", 5, 1, 4096, Duration::from_secs(1))
            .expect("template must render");
        let lower = html.to_lowercase();
        assert!(lower.contains("mmap"), "{html}");
        let pos = lower
            .find("resident")
            .unwrap_or_else(|| panic!("the page must have a resident-memory line: {html}"));
        let window = &lower[pos..(pos + 200).min(lower.len())];
        for unit in ["kb", "mb", "gb", " b)", " bytes"] {
            assert!(
                !window.contains(unit),
                "the resident-memory line must carry no byte figure (`{unit}`): {window}"
            );
        }
    }

    /// The stats page is read-only: no form, no submit control, nothing that
    /// could turn a page view into a mutation.
    #[test]
    fn stats_page_has_no_form() {
        let html = render_stats_page("content", 0, 0, 0, Duration::from_secs(0))
            .expect("template must render");
        assert!(!html.to_lowercase().contains("<form"), "{html}");
    }

    #[test]
    fn stats_page_escapes_the_core_name() {
        let html = render_stats_page("<script>", 0, 0, 0, Duration::from_secs(0))
            .expect("template must render");
        assert!(!html.contains("<script>"));
        assert!(html.contains("&#60;script&#62;"));
    }

    fn field(
        name: &str,
        type_: &str,
        stored: bool,
        fast: bool,
        multi: bool,
        req: bool,
    ) -> FieldConfig {
        FieldConfig {
            name: name.to_string(),
            type_: type_.to_string(),
            stored,
            required: req,
            fast,
            multi_valued: multi,
        }
    }

    /// Every `<td>` cell of every `<tr>` in `html`, in document order.
    ///
    /// Lets the assertions below pin *which* value lands in which column
    /// without pinning whitespace or the surrounding markup, so the table can
    /// be restyled without rewriting the tests.
    fn table_rows(html: &str) -> Vec<Vec<String>> {
        html.split("<tr>")
            .skip(1)
            .map(|row| {
                let row = row.split("</tr>").next().unwrap_or("");
                row.split("<td>")
                    .skip(1)
                    .filter_map(|cell| cell.split("</td>").next())
                    .map(|cell| cell.trim().to_string())
                    .collect()
            })
            .filter(|cells: &Vec<String>| !cells.is_empty())
            .collect()
    }

    fn sample_schema_page() -> String {
        let fields = vec![
            field("plain_field", "string", false, false, false, false),
            field("stored_field", "string", true, false, false, false),
            field("fast_field", "string", false, true, false, false),
            field("multi_field", "string", false, false, true, false),
            field("required_field", "string", false, false, false, true),
        ];
        let dynamic_fields = vec![
            DynamicFieldConfig {
                pattern: "*_s".to_string(),
                type_: "string".to_string(),
                stored: true,
                fast: true,
                multi_valued: false,
            },
            DynamicFieldConfig {
                pattern: "*_i".to_string(),
                type_: "long".to_string(),
                stored: true,
                fast: false,
                multi_valued: false,
            },
        ];
        let copy_fields = vec![CopyFieldConfig {
            source: "body".to_string(),
            dest: "body_copy".to_string(),
        }];
        render_schema_page("content", &fields, &dynamic_fields, &copy_fields)
            .expect("template must render")
    }

    /// Each flag must land in its own column, reflecting that field's real
    /// value: five fields differing in exactly one flag each, pinned cell by
    /// cell. `tests/admin_ui_schema_view.rs` only asserts that two such rows
    /// *differ*, which the differing field names alone already satisfy — this
    /// is what actually catches a swapped or hardcoded flag column.
    #[test]
    fn schema_page_renders_each_field_flag_in_its_own_column() {
        let html = sample_schema_page();
        let rows = table_rows(&html);
        let expected = [
            ["plain_field", "string", "no", "no", "no", "no"],
            ["stored_field", "string", "yes", "no", "no", "no"],
            ["fast_field", "string", "no", "yes", "no", "no"],
            ["multi_field", "string", "no", "no", "yes", "no"],
            ["required_field", "string", "no", "no", "no", "yes"],
        ];
        for row in expected {
            assert!(
                rows.contains(&row.iter().map(|c| c.to_string()).collect::<Vec<_>>()),
                "expected field row {row:?} in the rendered page; rows: {rows:?}"
            );
        }
    }

    #[test]
    fn schema_page_renders_dynamic_rules_with_their_own_type_and_flags() {
        let html = sample_schema_page();
        let rows = table_rows(&html);
        for row in [
            ["*_s", "string", "yes", "yes", "no"],
            ["*_i", "long", "yes", "no", "no"],
        ] {
            assert!(
                rows.contains(&row.iter().map(|c| c.to_string()).collect::<Vec<_>>()),
                "expected dynamic-field row {row:?}; rows: {rows:?}"
            );
        }
    }

    #[test]
    fn schema_page_renders_copy_fields_as_source_dest_pairs() {
        let html = sample_schema_page();
        let rows = table_rows(&html);
        assert!(
            rows.contains(&vec!["body".to_string(), "body_copy".to_string()]),
            "expected the copy-field pair row; rows: {rows:?}"
        );
    }

    #[test]
    fn schema_page_says_so_when_a_section_is_empty() {
        let fields = vec![field("id", "string", true, true, false, true)];
        let html = render_schema_page("content", &fields, &[], &[]).expect("template must render");
        assert_eq!(
            html.matches("none declared").count(),
            2,
            "both the dynamic-field and copy-field sections must say they are \
             empty rather than rendering a headerless table; {html}"
        );
    }

    /// Field names come from an operator-authored TOML, so they reach the page
    /// as untrusted text — same escaping contract as the core name.
    #[test]
    fn schema_page_escapes_field_names_and_types() {
        let fields = vec![field("<script>", "<b>", false, false, false, false)];
        let html = render_schema_page("content", &fields, &[], &[]).expect("template must render");
        assert!(!html.contains("<script>"), "{html}");
        assert!(html.contains("&#60;script&#62;"), "{html}");
        assert!(html.contains("&#60;b&#62;"), "{html}");
    }
}
