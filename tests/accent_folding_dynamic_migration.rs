//! Issue #389 Phase 2: dynamic folding and safe legacy-index migration.

mod common;

use common::app_with_schema;
use tantivy::Index;
use tempfile::TempDir;
use wayfinder::schema;

fn static_preset_schema(type_name: &str) -> String {
    format!(
        r#"
[core]
name = "legacy-{type_name}"
unique_key = "id"
default_field = "body"

[[fields]]
name = "id"
type = "string"
stored = true
required = true

[[fields]]
name = "body"
type = "{type_name}"
stored = true
"#
    )
}

#[test]
fn unmarked_pre_v3_static_text_presets_refuse_startup_and_require_reindex() {
    for (type_name, legacy_tokenizer) in [("text_general", "default"), ("text_de", "text_de")] {
        let toml = static_preset_schema(type_name);
        let dir = TempDir::new().expect("temp dir");
        let schema_path = dir.path().join("schema.toml");
        std::fs::write(&schema_path, &toml).expect("write legacy schema");
        let current = schema::load(&schema_path).expect("schema loads");
        let mut persisted = serde_json::to_value(&current.tantivy_schema)
            .expect("serialize current Tantivy schema");
        replace_tokenizer_for_field(&mut persisted, "body", legacy_tokenizer)
            .unwrap_or_else(|| panic!("legacy `{type_name}` tokenizer must be materialized"));

        let data_dir = dir.path().join("data");
        std::fs::create_dir_all(&data_dir).expect("create data dir");
        Index::builder()
            .schema(serde_json::from_value(persisted).expect("legacy Tantivy schema parses"))
            .create_in_dir(&data_dir)
            .expect("create real pre-v3 Tantivy index");
        std::fs::write(data_dir.join("wayfinder-schema.toml"), &toml)
            .expect("write schema snapshot");
        assert!(
            !data_dir.join("wayfinder-analyzer-contract").exists(),
            "test setup must model an unmarked pre-v3 index"
        );

        let err = app_with_schema(dir.path(), &toml)
            .expect_err("pre-v3 static text index must refuse startup rather than adopt old terms");
        assert!(
            format!("{err:#}").to_lowercase().contains("reindex"),
            "legacy `{type_name}` refusal must explicitly require reindexing: {err:#}"
        );
    }
}

#[test]
fn v4_static_text_presets_refuse_startup_for_uax29_reindexing() {
    // v4 has the folded-but-SimpleTokenizer identities. Its marker must fail
    // closed for every family of static preset that v5 moved to UAX #29,
    // including the legacy-dynamic marker variant.
    for (type_name, legacy_tokenizer) in [
        ("text_general", "wayfinder_text_general_v4"),
        ("text_en", "wayfinder_text_en_v4"),
        ("text_de", "wayfinder_text_de_v4"),
    ] {
        for marker in [
            "text_presets_accent_folding_v4",
            "text_presets_accent_folding_v4_legacy_dynamic_text",
        ] {
            let toml = static_preset_schema(type_name);
            let dir = TempDir::new().expect("temp dir");
            let schema_path = dir.path().join("schema.toml");
            std::fs::write(&schema_path, &toml).expect("write legacy schema");
            let current = schema::load(&schema_path).expect("schema loads");
            let mut persisted = serde_json::to_value(&current.tantivy_schema)
                .expect("serialize current Tantivy schema");
            replace_tokenizer_for_field(&mut persisted, "body", legacy_tokenizer)
                .unwrap_or_else(|| panic!("legacy `{type_name}` tokenizer must be materialized"));

            let data_dir = dir.path().join("data");
            std::fs::create_dir_all(&data_dir).expect("create data dir");
            Index::builder()
                .schema(serde_json::from_value(persisted).expect("legacy Tantivy schema parses"))
                .create_in_dir(&data_dir)
                .expect("create legacy index");
            std::fs::write(data_dir.join("wayfinder-schema.toml"), &toml)
                .expect("write schema snapshot");
            std::fs::write(data_dir.join("wayfinder-analyzer-contract"), marker)
                .expect("write v4 analyzer contract");

            let err = app_with_schema(dir.path(), &toml)
                .expect_err("a v4 static text preset index must require UAX #29 reindexing");
            assert!(
                format!("{err:#}").to_lowercase().contains("reindex"),
                "v4 `{type_name}` marker `{marker}` must refuse startup with reindexing: {err:#}"
            );
        }
    }
}

fn replace_tokenizer_for_field(
    value: &mut serde_json::Value,
    field_name: &str,
    legacy_tokenizer: &str,
) -> Option<()> {
    match value {
        serde_json::Value::Object(map)
            if map.get("name").and_then(serde_json::Value::as_str) == Some(field_name) =>
        {
            replace_tokenizer(value, legacy_tokenizer)
        }
        serde_json::Value::Object(map) => map
            .values_mut()
            .find_map(|child| replace_tokenizer_for_field(child, field_name, legacy_tokenizer)),
        serde_json::Value::Array(values) => values
            .iter_mut()
            .find_map(|child| replace_tokenizer_for_field(child, field_name, legacy_tokenizer)),
        _ => None,
    }
}

fn replace_tokenizer(value: &mut serde_json::Value, legacy_tokenizer: &str) -> Option<()> {
    match value {
        serde_json::Value::Object(map) => {
            if let Some(tokenizer) = map.get_mut("tokenizer") {
                *tokenizer = serde_json::Value::String(legacy_tokenizer.to_string());
                return Some(());
            }
            map.values_mut()
                .find_map(|child| replace_tokenizer(child, legacy_tokenizer))
        }
        serde_json::Value::Array(values) => values
            .iter_mut()
            .find_map(|child| replace_tokenizer(child, legacy_tokenizer)),
        _ => None,
    }
}
