//! Analyzer contract for the retired `text_en` accent folding (issue #389,
//! Phase 2). Issue #479 removed the folding so `text_en` matches Solr; the
//! behaviour is covered by `tests/text_en_solr_parity.rs`.

mod common;

use common::app_with_schema;
use tempfile::TempDir;

const ACCENT_SCHEMA_TOML: &str = r#"
[core]
name = "content"
unique_key = "id"
default_field = "body"

[[fields]]
name = "id"
type = "string"
stored = true
required = true
fast = true

[[fields]]
name = "body"
type = "text_en"
stored = true
"#;

#[test]
fn pre_folding_analyzer_contract_requires_reindex_after_the_term_format_change() {
    const PRE_FOLDING_CONTRACT: &str = "text_en_porter_compatible_v2";

    let dir = TempDir::new().expect("temp dir");
    let app = app_with_schema(dir.path(), ACCENT_SCHEMA_TOML).expect("fresh index must build");
    drop(app);
    std::fs::write(
        dir.path().join("data/wayfinder-analyzer-contract"),
        PRE_FOLDING_CONTRACT,
    )
    .expect("write pre-folding analyzer-contract marker");

    let err = app_with_schema(dir.path(), ACCENT_SCHEMA_TOML)
        .expect_err("a pre-folding text index must require reindexing");
    assert!(
        format!("{err:#}").to_lowercase().contains("reindex"),
        "the pre-folding marker refusal must require reindexing, got: {err:#}"
    );
}
