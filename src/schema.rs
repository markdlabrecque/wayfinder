//! TOML schema file -> Tantivy `Schema` (PRD §3 / §7).
//!
//! Tracer-bullet scope: `string` and `text_en` field types only, with
//! `stored`, `required`, `fast`, `multi_valued` options. Everything else in
//! the full PRD schema format (dynamic fields, copy fields, custom analyzer
//! chains, other field types) is out of scope for this slice.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use tantivy::schema::{
    Field, IndexRecordOption, STRING, Schema, TextFieldIndexing, TextOptions,
};

#[derive(Debug, Deserialize)]
struct SchemaFile {
    core: CoreConfig,
    #[serde(rename = "fields", default)]
    fields: Vec<FieldConfig>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct CoreConfig {
    pub name: String,
    pub unique_key: String,
    pub default_field: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct FieldConfig {
    pub name: String,
    #[serde(rename = "type")]
    pub type_: String,
    #[serde(default)]
    pub stored: bool,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub fast: bool,
    #[serde(default)]
    pub multi_valued: bool,
}

/// A parsed schema: the Tantivy `Schema`, the core config, and a lookup from
/// field name to both the Tantivy `Field` handle and the original config
/// (needed to know stored-ness / multi-valuedness when rendering docs).
pub struct WayfinderSchema {
    pub tantivy_schema: Schema,
    pub core: CoreConfig,
    pub fields: Vec<FieldConfig>,
    pub field_handles: HashMap<String, Field>,
}

impl WayfinderSchema {
    pub fn field(&self, name: &str) -> Option<Field> {
        self.field_handles.get(name).copied()
    }

}

/// Loads and builds a Tantivy schema from a TOML schema file at `path`.
pub fn load(path: &Path) -> Result<WayfinderSchema> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("reading schema file {}", path.display()))?;
    let parsed: SchemaFile =
        toml::from_str(&raw).with_context(|| format!("parsing schema file {}", path.display()))?;

    let mut builder = Schema::builder();
    let mut field_handles = HashMap::new();

    for field_config in &parsed.fields {
        let field = match field_config.type_.as_str() {
            "string" => {
                let mut opts = STRING;
                if field_config.stored {
                    opts = opts.set_stored();
                }
                if field_config.fast {
                    opts = opts.set_fast(None);
                }
                builder.add_text_field(&field_config.name, opts)
            }
            "text_en" => {
                let indexing = TextFieldIndexing::default()
                    .set_tokenizer("en_stem")
                    .set_index_option(IndexRecordOption::WithFreqsAndPositions);
                let mut opts = TextOptions::default().set_indexing_options(indexing);
                if field_config.stored {
                    opts = opts.set_stored();
                }
                if field_config.fast {
                    opts = opts.set_fast(Some("en_stem"));
                }
                builder.add_text_field(&field_config.name, opts)
            }
            other => bail!("unsupported field type `{other}` on field `{}`", field_config.name),
        };
        field_handles.insert(field_config.name.clone(), field);
    }

    let tantivy_schema = builder.build();

    if !field_handles.contains_key(&parsed.core.unique_key) {
        bail!(
            "core.unique_key `{}` is not a declared field",
            parsed.core.unique_key
        );
    }
    if !field_handles.contains_key(&parsed.core.default_field) {
        bail!(
            "core.default_field `{}` is not a declared field",
            parsed.core.default_field
        );
    }

    Ok(WayfinderSchema {
        tantivy_schema,
        core: parsed.core,
        fields: parsed.fields,
        field_handles,
    })
}
