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

use askama::Template;

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

#[cfg(test)]
mod tests {
    use super::*;

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
}
