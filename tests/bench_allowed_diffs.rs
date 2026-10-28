//! The bench correctness gate can't pass with unexplained differences (issue
//! #443). `bool_or` and `edismax` find a few more pages than Solr on 20,000
//! docs, so each must be waived in `bench/allowed-diffs.tsv` with a reason that
//! lives in `docs/COMPATIBILITY.md`, and every waiver must point at a real
//! query and a real section. (Lives here because the bench crate's own tests
//! are not run by the root `cargo test`.)

use std::path::PathBuf;

fn read(rel: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// (query name, section heading) for each waiver line.
fn waivers() -> Vec<(String, String)> {
    read("bench/allowed-diffs.tsv")
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
        .map(|l| {
            let mut cols = l.split('\t');
            let name = cols.next().unwrap_or_default().trim().to_string();
            let section = cols.next().unwrap_or_default().trim().to_string();
            (name, section)
        })
        .collect()
}

fn heading_text(s: &str) -> &str {
    s.trim_start_matches('#').trim()
}

/// The text under the heading named `section`, up to the next `## ` heading.
fn section_body(section: &str) -> Option<String> {
    let doc = read("docs/COMPATIBILITY.md");
    let mut lines = doc.lines();
    lines.find(|l| l.starts_with('#') && heading_text(l) == heading_text(section))?;
    let body: Vec<&str> = lines.take_while(|l| !l.starts_with("## ")).collect();
    Some(body.join("\n"))
}

#[test]
fn bool_or_and_edismax_extra_pages_are_waived() {
    let listed = waivers();
    for query in ["bool_or", "edismax"] {
        assert!(
            listed.iter().any(|(name, _)| name == query),
            "{query} finds extra pages vs Solr but is not in bench/allowed-diffs.tsv"
        );
    }
}

#[test]
fn every_waiver_names_a_real_query_and_a_section_that_explains_it() {
    let queries = read("bench/queries.tsv");
    let known: Vec<&str> = queries
        .lines()
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| l.split('\t').next())
        .collect();
    let listed = waivers();
    assert!(!listed.is_empty(), "no waivers listed");
    for (name, section) in listed {
        assert!(known.contains(&name.as_str()), "{name} is not in queries.tsv");
        let body = section_body(&section)
            .unwrap_or_else(|| panic!("{name}: no section {section:?} in docs/COMPATIBILITY.md"));
        assert!(
            body.contains(&name),
            "{name}: section {section:?} does not mention the query, so gives no reason"
        );
    }
}
