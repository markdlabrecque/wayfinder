//! The complete `/update/extract` workflow behind the multipart adapter.
//!
//! The HTTP module identifies multipart parts and adapts each part to
//! [`ChunkSource`]. This module owns admission, request-wide accounting,
//! temporary storage, resident read-back, parsing, Solr Cell mapping, and the
//! index-or-render decision.

use std::collections::HashMap;

use serde_json::{Map, Value, json};
use tempfile::NamedTempFile;

use crate::core_index::CoreIndex;
use crate::error::{Envelope, WfError};
use crate::extract::{
    self, ChunkSource, ExtractError, ExtractLimits, ExtractedDocument, ExtractionRuntime,
    InflightUploadPermit,
};
use crate::facet;
use crate::params::Params;
use crate::schema::WayfinderSchema;
use crate::update_policy::UpdatePolicy;

const EXTRACT_DEFAULT_FMAP: &[(&str, &str)] = &[("a", "links"), ("div", "ignored_")];
const EXTRACT_DEFAULT_UPREFIX: &str = "ignored_";

/// Metadata the multipart adapter preserves for the selected file part.
pub(crate) struct UploadMetadata {
    pub part_name: String,
    pub file_name: String,
    pub declared_type: String,
}

/// Request intake under one in-flight permit and one byte counter.
///
/// The permit is acquired before the adapter consumes a part. It remains in
/// this value through temporary-file read-back and parsing, then the workflow
/// drops it before rendering, indexing, or committing.
pub(crate) struct Intake {
    temp: NamedTempFile,
    consumed: u64,
    max_body_bytes: u64,
    inflight: InflightUploadPermit,
}

impl Intake {
    pub(crate) fn new(
        runtime: &ExtractionRuntime,
        limits: ExtractLimits,
    ) -> Result<Self, ExtractError> {
        // Preserve the old route's order: allocate the temporary file, then
        // claim intake capacity, and consume no body until both succeed.
        let temp = NamedTempFile::new()
            .map_err(|error| ExtractError::Io(format!("creating upload temp file: {error}")))?;
        let inflight = runtime
            .try_acquire_inflight()
            .ok_or(ExtractError::InflightUploadsBusy)?;
        Ok(Self {
            temp,
            consumed: 0,
            max_body_bytes: limits.max_body_bytes,
            inflight,
        })
    }

    /// Discards a non-file part while charging it to the request total.
    pub(crate) async fn drain(
        &mut self,
        source: &mut impl ChunkSource,
    ) -> Result<u64, ExtractError> {
        extract::drain_counted(source, self.max_body_bytes, &mut self.consumed).await
    }

    /// Stores the selected file part and transfers the admission permit to
    /// the resident-upload phase.
    pub(crate) async fn store(
        mut self,
        source: &mut impl ChunkSource,
    ) -> Result<ResidentUpload, ExtractError> {
        let stream_size = extract::stream_to_tempfile_counted(
            source,
            &mut self.temp,
            self.max_body_bytes,
            &mut self.consumed,
        )
        .await?;
        Ok(ResidentUpload {
            temp: self.temp,
            stream_size,
            inflight: self.inflight,
        })
    }
}

/// A counted upload whose in-flight permit remains held while bytes may be
/// resident. Callers cannot release that permit separately from the upload.
pub(crate) struct ResidentUpload {
    temp: NamedTempFile,
    stream_size: u64,
    inflight: InflightUploadPermit,
}

/// The extraction workflow. Callers supply multipart parts through
/// [`Intake`], then hand the selected upload back through [`Self::finish`].
pub(crate) struct Workflow<'a> {
    runtime: &'a ExtractionRuntime,
    index: &'a CoreIndex,
    limits: ExtractLimits,
    params: &'a Params,
    extract_only: bool,
    as_text: bool,
}

impl<'a> Workflow<'a> {
    pub(crate) fn new(
        runtime: &'a ExtractionRuntime,
        index: &'a CoreIndex,
        limits: ExtractLimits,
        params: &'a Params,
    ) -> Result<Self, WfError> {
        let extract_only = params
            .bool_or("extractOnly", false)
            .map_err(|error| error.envelope(Envelope::NoParams))?;
        let as_text = if extract_only {
            match params.get("extractFormat") {
                None | Some("xml") => false,
                Some("text") => true,
                Some(other) => {
                    return Err(bad_request(
                        params,
                        "wayfinder::InvalidParam",
                        format!("invalid extractFormat value: {other}"),
                    ));
                }
            }
        } else {
            false
        };
        Ok(Self {
            runtime,
            index,
            limits,
            params,
            extract_only,
            as_text,
        })
    }

    pub(crate) fn begin(&self) -> Result<Intake, WfError> {
        Intake::new(self.runtime, self.limits)
            .map_err(|error| WfError::from(error).with_params(self.params))
    }

    /// Parses the upload and either renders the extract-only document or
    /// maps and indexes it. Upload capacity is released immediately after
    /// parsing, before either branch performs later work.
    pub(crate) async fn finish(
        self,
        upload: ResidentUpload,
        metadata: UploadMetadata,
    ) -> Result<Value, WfError> {
        let resource_name = self
            .params
            .get("resource.name")
            .unwrap_or(&metadata.file_name)
            .to_string();
        let ResidentUpload {
            temp,
            stream_size,
            inflight,
        } = upload;
        let bytes = std::fs::read(temp.path()).map_err(|error| {
            WfError::internal(
                "wayfinder::ExtractionIo",
                format!("reading upload temp file: {error}"),
            )
            .with_params(self.params)
            .envelope(Envelope::NoParams)
        })?;

        let job_type = metadata.declared_type.clone();
        let job_resource = resource_name.clone();
        let limits = self.limits;
        let doc = self
            .runtime
            .spawn_extraction(limits.deadline, move || {
                // Tie intake admission to the resident byte buffer. The slot
                // cannot be released before parsing without moving this guard
                // out of the same closure that owns `bytes`.
                let _inflight = inflight;
                let budget = extract::Budget::new(limits);
                extract::extract_document(Some(&job_type), &job_resource, &bytes, &budget)
            })
            .await
            .and_then(|result| result)
            .map_err(|error| WfError::from(error).with_params(self.params))?;

        if self.extract_only {
            Ok(self.render_extract_only(&doc, &metadata, &resource_name, stream_size))
        } else {
            self.index_document(&doc)?;
            Ok(success_body(self.params))
        }
    }

    fn index_document(&self, doc: &ExtractedDocument) -> Result<(), WfError> {
        // Index-only params remain inert on `extractOnly=true`, and malformed
        // documents fail before these are validated, matching the old route.
        let update_policy = UpdatePolicy::from_params(self.params)?;
        let fields = extract_cell_fields(doc, self.params, &self.index.wf_schema)
            .map_err(|error| error.envelope(Envelope::NoParams))?;
        let mut object = Map::new();
        for (name, mut values) in fields {
            let value = if values.len() == 1 {
                values.pop().expect("len == 1")
            } else {
                Value::Array(values)
            };
            object.insert(name, value);
        }
        self.index
            .add_documents(&[Value::Object(object)], update_policy.overwrite)
            .map_err(|error| {
                bad_request(self.params, "wayfinder::IndexError", error.to_string())
            })?;
        update_policy.finish(self.index).map_err(|error| {
            WfError::internal("wayfinder::CommitError", error.to_string())
                .with_params(self.params)
                .envelope(Envelope::NoParams)
        })
    }

    fn render_extract_only(
        &self,
        doc: &ExtractedDocument,
        metadata: &UploadMetadata,
        resource_name: &str,
        stream_size: u64,
    ) -> Value {
        let render = extract::ExtractRender {
            part_name: &metadata.part_name,
            resource_name,
            stream_source_info: &metadata.file_name,
            declared_type: &metadata.declared_type,
            stream_size,
            doc,
        };
        let file = if self.as_text {
            render.text()
        } else {
            render.xhtml()
        };
        let entries: Vec<(String, Value)> = render
            .file_metadata()
            .into_iter()
            .map(|(key, values)| {
                (
                    key,
                    Value::Array(values.into_iter().map(Value::String).collect()),
                )
            })
            .collect();
        let file_metadata =
            facet::render_named_list(&entries, facet::JsonNl::from_params(self.params));
        let mut body = Map::new();
        if !self.params.omit_header() {
            body.insert(
                "responseHeader".to_string(),
                json!({"status": 0, "QTime": 0}),
            );
        }
        body.insert("file".to_string(), Value::String(file));
        body.insert("file_metadata".to_string(), file_metadata);
        Value::Object(body)
    }
}

fn bad_request(params: &Params, class: &'static str, message: String) -> WfError {
    WfError::bad_request(class, message)
        .with_params(params)
        .envelope(Envelope::NoParams)
}

fn success_body(params: &Params) -> Value {
    if params.omit_header() {
        json!({})
    } else {
        json!({"responseHeader": {"status": 0, "QTime": 0}})
    }
}

/// Applies Solr Cell's `lowernames`, `fmap`, `uprefix`, and `literal.*`
/// sequence to extracted fields.
fn extract_cell_fields(
    doc: &ExtractedDocument,
    params: &Params,
    schema: &WayfinderSchema,
) -> Result<Vec<(String, Vec<Value>)>, WfError> {
    let lowernames = params.bool_or("lowernames", true)?;
    let capture_attr = params.bool_or("captureAttr", true)?;
    let uprefix = params.get("uprefix").unwrap_or(EXTRACT_DEFAULT_UPREFIX);
    let uprefix_set = !uprefix.is_empty();

    let mut fmap: HashMap<&str, &str> = HashMap::new();
    for (from, to) in EXTRACT_DEFAULT_FMAP {
        fmap.insert(from, to);
    }
    for (from, to) in params.pairs_with_prefix("fmap.") {
        fmap.insert(from, to);
    }
    let rename = |name: &str| {
        fmap.get(name)
            .map(|value| (*value).to_string())
            .unwrap_or_else(|| name.to_string())
    };
    let resolves = |name: &str| schema.is_static(name) || schema.match_dynamic(name).is_some();

    let mut source = doc.extract_source_fields();
    if capture_attr {
        for (element, value) in &doc.captured_attrs {
            if let Some(entry) = source.iter_mut().find(|(name, _)| name == element) {
                entry.1.push(value.clone());
            } else {
                source.push((element.clone(), vec![value.clone()]));
            }
        }
    }

    let mut fields: Vec<(String, Vec<Value>)> = Vec::new();
    let push = |raw_name: String, values: Vec<String>, fields: &mut Vec<(String, Vec<Value>)>| {
        let name = if lowernames {
            raw_name.to_ascii_lowercase()
        } else {
            raw_name
        };
        let name = rename(&name);
        if !resolves(&name) && uprefix_set {
            return;
        }
        let values = values.into_iter().map(Value::String).collect::<Vec<_>>();
        if let Some(entry) = fields.iter_mut().find(|(field, _)| *field == name) {
            entry.1.extend(values);
        } else {
            fields.push((name, values));
        }
    };
    for (name, values) in source {
        push(name, values, &mut fields);
    }
    for (field, value) in params.pairs_with_prefix("literal.") {
        push(field.to_string(), vec![value.to_string()], &mut fields);
    }
    Ok(fields)
}

#[cfg(test)]
mod tests {
    use axum::body::Bytes;
    use tempfile::TempDir;

    use crate::config::ServerConfig;
    use crate::extract::{ChunkSource, ExtractError, ExtractLimits, ExtractionRuntime};
    use crate::params::Params;

    use super::{Intake, UploadMetadata, Workflow};

    const SCHEMA: &str = r#"
[core]
name = "content"
unique_key = "id"
default_field = "body"

[[fields]]
name = "id"
type = "string"
stored = true
required = true

[[fields]]
name = "body"
type = "text_en"
stored = true
"#;

    struct Chunks(Vec<Bytes>);

    impl ChunkSource for Chunks {
        async fn next_chunk(&mut self) -> Option<std::io::Result<Bytes>> {
            if self.0.is_empty() {
                None
            } else {
                Some(Ok(self.0.remove(0)))
            }
        }
    }

    fn limits(max_body_bytes: u64, max_inflight_uploads: usize) -> ExtractLimits {
        ExtractLimits {
            max_body_bytes,
            max_inflight_uploads,
            ..ExtractLimits::default()
        }
    }

    fn test_server() -> (crate::AppServer, TempDir) {
        let dir = TempDir::new().expect("temp dir");
        let schema = dir.path().join("schema.toml");
        std::fs::write(&schema, SCHEMA).expect("write schema");
        let data = dir.path().join("data");
        std::fs::create_dir(&data).expect("create data dir");
        let server = crate::build(&schema, &data, ServerConfig::default()).expect("build app");
        (server, dir)
    }

    async fn stored_upload(workflow: &Workflow<'_>, bytes: &'static [u8]) -> super::ResidentUpload {
        workflow
            .begin()
            .expect("admit intake")
            .store(&mut Chunks(vec![Bytes::from_static(bytes)]))
            .await
            .expect("store upload")
    }

    fn metadata(name: &str, declared_type: &str) -> UploadMetadata {
        UploadMetadata {
            part_name: "file".to_string(),
            file_name: name.to_string(),
            declared_type: declared_type.to_string(),
        }
    }

    #[tokio::test]
    async fn workflow_renders_extract_only_documents() {
        let (server, _dir) = test_server();
        let state = &server.shutdown.0;
        let params = Params::parse("extractOnly=true&extractFormat=text").allow_omit_header();
        let workflow = Workflow::new(
            &state.extraction,
            &state.index,
            state.extract_limits,
            &params,
        )
        .expect("build workflow");
        let upload = stored_upload(&workflow, b"workflow text").await;

        let body = workflow
            .finish(upload, metadata("sample.txt", "text/plain"))
            .await
            .expect("extract document");

        assert!(
            body["file"]
                .as_str()
                .is_some_and(|file| file.contains("workflow text"))
        );
        assert!(body["file_metadata"].is_array());
    }

    #[tokio::test]
    async fn workflow_maps_indexes_and_commits_extracted_documents() {
        let (server, _dir) = test_server();
        let state = &server.shutdown.0;
        let params =
            Params::parse("literal.id=direct&fmap.content=body&commit=true").allow_omit_header();
        let workflow = Workflow::new(
            &state.extraction,
            &state.index,
            state.extract_limits,
            &params,
        )
        .expect("build workflow");
        let upload = stored_upload(&workflow, b"indexed workflow text").await;

        workflow
            .finish(upload, metadata("sample.txt", "text/plain"))
            .await
            .expect("index document");

        let query = state
            .index
            .parse_query("id:direct", "body")
            .expect("parse id query");
        assert_eq!(
            state
                .index
                .search(query.as_ref(), &[], &[])
                .expect("search")
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn workflow_releases_intake_after_an_unsupported_document() {
        let (server, _dir) = test_server();
        let state = &server.shutdown.0;
        let params = Params::parse("extractOnly=true").allow_omit_header();
        let workflow = Workflow::new(
            &state.extraction,
            &state.index,
            state.extract_limits,
            &params,
        )
        .expect("build workflow");
        let upload = stored_upload(&workflow, b"\x89PNG\r\n\x1a\n").await;

        assert!(
            workflow
                .finish(upload, metadata("image.png", "image/png"))
                .await
                .is_err()
        );
        assert!(state.extraction.try_acquire_inflight().is_some());
    }

    #[tokio::test]
    async fn intake_counts_all_parts_against_one_request_budget() {
        let limits = limits(100, 1);
        let runtime = ExtractionRuntime::new(&limits);
        let mut intake = Intake::new(&runtime, limits).expect("admit intake");

        intake
            .drain(&mut Chunks(vec![Bytes::from(vec![b'm'; 60])]))
            .await
            .expect("first part fits");
        let result = intake
            .store(&mut Chunks(vec![Bytes::from(vec![b'f'; 60])]))
            .await;

        assert!(matches!(
            result,
            Err(ExtractError::BodyTooLarge { limit: 100 })
        ));
    }

    #[tokio::test]
    async fn resident_upload_keeps_the_intake_permit() {
        let limits = limits(100, 1);
        let runtime = ExtractionRuntime::new(&limits);
        let intake = Intake::new(&runtime, limits).expect("admit intake");
        let upload = intake
            .store(&mut Chunks(vec![Bytes::from_static(b"document")]))
            .await
            .expect("store upload");

        assert!(
            runtime.try_acquire_inflight().is_none(),
            "storing the body must not release its resident-memory slot"
        );
        drop(upload);
        assert!(runtime.try_acquire_inflight().is_some());
    }

    #[tokio::test]
    async fn intake_permit_returns_on_error_paths() {
        let limits = limits(4, 1);
        let runtime = ExtractionRuntime::new(&limits);
        let intake = Intake::new(&runtime, limits).expect("admit intake");
        let result = intake
            .store(&mut Chunks(vec![Bytes::from_static(b"too large")]))
            .await;
        assert!(matches!(result, Err(ExtractError::BodyTooLarge { .. })));

        assert!(runtime.try_acquire_inflight().is_some());
    }
}
