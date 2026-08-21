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
use crate::update_policy::{UpdatePolicy, success_body};

/// The Search-API configset's `ExtractingRequestHandler` defaults, hardcoded
/// because they are the only evidenced config -- the captured index/select pair
/// was taken against exactly these. Request params override and extend them: a
/// request `fmap.<from>` wins over the default on the same `<from>` and adds
/// new mappings, while `lowernames`/`uprefix`/`captureAttr` are overridden
/// outright when sent.
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
        params: &Params,
    ) -> Result<Self, WfError> {
        // Preserve the old route's order: allocate the temporary file, then
        // claim intake capacity, and consume no body until both succeed.
        let temp = NamedTempFile::new().map_err(|error| {
            extraction_io(params, format!("creating upload temp file: {error}"))
        })?;
        let inflight = runtime.try_acquire_inflight().ok_or_else(|| {
            WfError::from(ExtractError::InflightUploadsBusy)
                .with_params(params)
                .envelope(Envelope::NoParams)
        })?;
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

impl ResidentUpload {
    /// How many bytes the selected part actually streamed.
    fn stream_size(&self) -> u64 {
        self.stream_size
    }

    /// Reads the temporary file back into memory, pairing the buffer with the
    /// permit that bounds it. The temporary file is no longer needed once the
    /// bytes are resident, so it is removed here rather than at the end of the
    /// request.
    async fn read(self, params: &Params) -> Result<ResidentBytes, WfError> {
        // ponytail: the document is streamed to a temp file and then read back
        // whole, because `Extractor::extract` takes `&[u8]`. Bounded by
        // `extraction.max_body_bytes` (32 MiB by default), so it is a real
        // ceiling per request rather than an unbounded one -- and bounded across
        // requests by the in-flight-upload count `Intake` acquired (issue #273),
        // so total resident bytes are `max_inflight_uploads x max_body_bytes`,
        // not `max_body_bytes x HTTP concurrency`.
        //
        // What remains is that this is still a *full copy* in RAM, and the temp
        // file is currently only buying the streaming *count*. Trigger: the
        // first extractor that can work incrementally (the phase-2a ZIP walker,
        // per `ZipBudget`'s documented call sequence) wants a reader, at which
        // point `ExtractInput` grows a stream variant and this read goes away.
        let result = tokio::task::spawn_blocking(move || {
            let bytes = std::fs::read(self.temp.path())?;
            Ok::<_, std::io::Error>(ResidentBytes {
                bytes,
                _inflight: self.inflight,
            })
        })
        .await
        .map_err(|error| {
            extraction_io(
                params,
                format!("joining upload temp-file read task: {error}"),
            )
        })?;
        result.map_err(|error| extraction_io(params, format!("reading upload temp file: {error}")))
    }
}

/// The upload's resident bytes and the intake permit that bounds them, as one
/// value. The permit is private and has no accessor, so upload capacity cannot
/// be released while the buffer it accounts for is still readable: whatever
/// owns the bytes through the parse owns the permit for exactly as long.
pub(crate) struct ResidentBytes {
    bytes: Vec<u8>,
    _inflight: InflightUploadPermit,
}

impl ResidentBytes {
    fn as_slice(&self) -> &[u8] {
        &self.bytes
    }
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
        Intake::new(self.runtime, self.limits, self.params)
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
        let stream_size = upload.stream_size();
        let resident = upload.read(self.params).await?;

        let job_type = metadata.declared_type.clone();
        let job_resource = resource_name.clone();
        let limits = self.limits;
        let doc = self
            .runtime
            .spawn_extraction(limits.deadline, move || {
                let budget = extract::Budget::new(limits);
                let bytes = resident.as_slice();
                extract::extract_document(Some(&job_type), &job_resource, bytes, &budget)
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
        let fields = extract_cell_fields(doc, self.params, &self.index.wf_schema)?;
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

/// The one owner of the temporary-file I/O error class. Both halves of that
/// file's lifetime -- creating it during intake and reading it back before the
/// parse -- report `wayfinder::ExtractionIo`.
fn extraction_io(params: &Params, message: String) -> WfError {
    WfError::internal("wayfinder::ExtractionIo", message)
        .with_params(params)
        .envelope(Envelope::NoParams)
}

fn bad_request(params: &Params, class: &'static str, message: String) -> WfError {
    WfError::bad_request(class, message)
        .with_params(params)
        .envelope(Envelope::NoParams)
}

/// Applies Solr Cell's `lowernames`, `fmap`, `uprefix`, and `literal.*`
/// sequence to extracted fields.
///
/// A field that does not resolve against the schema is dropped when `uprefix`
/// is set -- reproducing the observable effect of the Search-API configset's
/// catch-all `<dynamicField name="*" type="ignored">` (stored/indexed false),
/// which is what makes `uprefix=ignored_` drop unmapped fields from selects.
/// Without `uprefix`, the field passes through so `add_documents` errors on a
/// genuinely unknown field exactly as strict Solr
/// (`-Dupdate.autoCreateFields=false`) does.
///
/// ponytail: Wayfinder drops uprefix'd fields outright rather than indexing
/// them into a catch-all ignored-type field. The observable result is
/// identical (the field never appears in a select); reproducing the
/// ignored-type field would need a schema change for no wire benefit. Trigger:
/// a captured index whose select returns a value Solr stored under the
/// catch-all but Wayfinder dropped.
///
/// The indexed `body`/`links` values come from Wayfinder's own extractors and
/// so diverge from the captured select fixture (`extract_html_select.json`):
/// Wayfinder does not replicate Tika's content-field whitespace, and PRD
/// divergence 10 forbids fabricating `shape="rect"`, so `links` carries only
/// the real attribute values. That divergence is recorded in the PRD and
/// asserted by the route tests; this function is where it originates.
fn extract_cell_fields(
    doc: &ExtractedDocument,
    params: &Params,
    schema: &WayfinderSchema,
) -> Result<Vec<(String, Vec<Value>)>, WfError> {
    let bool_param = |key: &str, default: bool| {
        params
            .bool_or(key, default)
            .map_err(|error| error.envelope(Envelope::NoParams))
    };
    let lowernames = bool_param("lowernames", true)?;
    let capture_attr = bool_param("captureAttr", true)?;
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
    use axum::body::{Bytes, to_bytes};
    use axum::response::IntoResponse;
    use tempfile::TempDir;

    use crate::config::{Extraction, ServerConfig};
    use crate::extract::{ChunkSource, ExtractError, ExtractLimits, ExtractionRuntime};
    use crate::params::Params;

    use super::{Intake, UploadMetadata, Workflow, extract_cell_fields};

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

    fn test_server(config: ServerConfig) -> (crate::AppServer, TempDir) {
        let dir = TempDir::new().expect("temp dir");
        let schema = dir.path().join("schema.toml");
        std::fs::write(&schema, SCHEMA).expect("write schema");
        let data = dir.path().join("data");
        std::fs::create_dir(&data).expect("create data dir");
        let server = crate::build(&schema, &data, config).expect("build app");
        (server, dir)
    }

    /// One intake slot, so acquiring it is observable: with the default 8 a
    /// permit that is leaked, or released too early, leaves a slot free either
    /// way and nothing can be concluded from `try_acquire_inflight`.
    fn one_upload_slot() -> ServerConfig {
        ServerConfig {
            extraction: Extraction {
                max_inflight_uploads: 1,
                ..Extraction::default()
            },
            ..ServerConfig::default()
        }
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
        let (server, _dir) = test_server(ServerConfig::default());
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
        let (server, _dir) = test_server(ServerConfig::default());
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

        let matches = |query_str: &str| {
            let query = state
                .index
                .parse_query(query_str, "body")
                .expect("parse query");
            state
                .index
                .search(query.as_ref(), &[], &[])
                .expect("search")
                .len()
        };
        // The `literal.*` leg, and then the extracted-content leg: dropping the
        // `extract_source_fields` mapping still indexes `{"id":"direct"}`, so
        // only the mapped `body` catches a bypassed content mapping.
        assert_eq!(matches("id:direct"), 1);
        assert_eq!(matches("body:\"indexed workflow text\""), 1);
    }

    #[tokio::test]
    async fn workflow_releases_intake_after_a_successful_parse() {
        let (server, _dir) = test_server(one_upload_slot());
        let state = &server.shutdown.0;
        let params = Params::parse("extractOnly=true").allow_omit_header();
        let workflow = Workflow::new(
            &state.extraction,
            &state.index,
            state.extract_limits,
            &params,
        )
        .expect("build workflow");
        let upload = stored_upload(&workflow, b"workflow text").await;

        assert!(
            state.extraction.try_acquire_inflight().is_none(),
            "the resident upload must still own the only intake slot"
        );

        workflow
            .finish(upload, metadata("sample.txt", "text/plain"))
            .await
            .expect("extract document");

        assert!(
            state.extraction.try_acquire_inflight().is_some(),
            "a successful parse must hand the intake slot back"
        );
    }

    #[tokio::test]
    async fn workflow_releases_intake_after_an_unsupported_document() {
        let (server, _dir) = test_server(one_upload_slot());
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
            state.extraction.try_acquire_inflight().is_none(),
            "the resident upload must still own the only intake slot"
        );

        assert!(
            workflow
                .finish(upload, metadata("image.png", "image/png"))
                .await
                .is_err()
        );

        assert!(
            state.extraction.try_acquire_inflight().is_some(),
            "a failed parse must hand the intake slot back"
        );
    }

    #[tokio::test]
    async fn extract_cell_boolean_errors_own_the_no_params_envelope() {
        let (server, _dir) = test_server(ServerConfig::default());
        let state = &server.shutdown.0;
        let limits = ExtractLimits::default();
        let budget = crate::extract::Budget::new(limits);
        let doc = crate::extract::extract_document(
            Some("text/plain"),
            "sample.txt",
            b"workflow text",
            &budget,
        )
        .expect("extract test document");

        for query in ["lowernames=maybe", "captureAttr=maybe"] {
            let params = Params::parse(query).allow_omit_header();
            let response = extract_cell_fields(&doc, &params, &state.index.wf_schema)
                .expect_err("invalid boolean must fail")
                .into_response();
            let bytes = to_bytes(response.into_body(), usize::MAX)
                .await
                .expect("read error response");
            let body: serde_json::Value = serde_json::from_slice(&bytes).expect("valid JSON error");

            assert!(
                body["responseHeader"].get("params").is_none(),
                "{query} must use the update-style NoParams envelope: {body}"
            );
        }
    }

    #[tokio::test]
    async fn intake_counts_all_parts_against_one_request_budget() {
        let limits = limits(100, 1);
        let runtime = ExtractionRuntime::new(&limits);
        let params = Params::parse("");
        let mut intake = Intake::new(&runtime, limits, &params).expect("admit intake");

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
        let params = Params::parse("");
        let intake = Intake::new(&runtime, limits, &params).expect("admit intake");
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
        let params = Params::parse("");
        let intake = Intake::new(&runtime, limits, &params).expect("admit intake");
        let result = intake
            .store(&mut Chunks(vec![Bytes::from_static(b"too large")]))
            .await;
        assert!(matches!(result, Err(ExtractError::BodyTooLarge { .. })));

        assert!(runtime.try_acquire_inflight().is_some());
    }
}
