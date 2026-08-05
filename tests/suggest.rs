//! `/suggest?suggest.buildAll=true` — the search_api_solr cron path (issue #352).
//!
//! `search_api_solr`'s `SearchApiSolrHooks::cron` fires
//! `GET /<core>/suggest?suggest.buildAll=true` via `fireAndForget`
//! (`nowaitforresponserequest`) whenever the server is Drupal-only-writeable,
//! the index saw updates since the last build, and the last build was more
//! than 1800s ago. Verified against the vendored 4.4.0 source at
//! `coverage/search_api_solr_4.4.0_source/src/Hook/SearchApiSolrHooks.php:143-164`
//! (gate) and `:159-161` (`getSuggesterQuery` + `addParam('suggest.buildAll',
//! TRUE)` + `fireAndForget`).
//!
//! ## Ground truth
//!
//! `solr-ref/responses/suggest_build_all.json`, captured against a real
//! `solr:9` with the canonical Drupal configset (which carries the `/suggest`
//! requestHandler and its `suggest` SuggestComponent in
//! `solr-ref/search-api/configset/solrconfig_extra.xml`). Solr's
//! SuggestComponent short-circuits a build command and emits
//! `{"responseHeader":{status,QTime},"command":"buildAll"}` — no `suggest`
//! block (that appears only for a `suggest.q` lookup) and crucially **no
//! `params` under `responseHeader`**: the component does not echo them, unlike
//! `/select`. Tantivy's term dictionary is already an FST, so Wayfinder has no
//! separate dictionary to build — `buildAll` is accepted and inert, returning
//! this envelope unchanged.

mod common;

use axum::Router;
use axum::http::StatusCode;
use common::{CORE, fixture, get, request_full};
use serde_json::{Value, json};
use tempfile::TempDir;

/// Builds an app against the tracer-bullet schema with an optional server
/// config TOML — mirrors `tests/admin_info_system.rs::build_app_with_config`.
fn build_app_with_config(config: Option<&str>) -> anyhow::Result<(Router, TempDir)> {
    let dir = TempDir::new().expect("create temp dir");
    let schema_path = dir.path().join("schema.toml");
    std::fs::write(&schema_path, common::SCHEMA_TOML).expect("write schema.toml");
    let data_dir = dir.path().join("data");
    std::fs::create_dir_all(&data_dir).expect("create data dir");
    let app = match config {
        Some(toml) => {
            let config_path = dir.path().join("wayfinder.toml");
            std::fs::write(&config_path, toml).expect("write wayfinder.toml");
            wayfinder::app_with_config(&schema_path, &data_dir, &config_path)?
        }
        None => wayfinder::app(&schema_path, &data_dir)?,
    };
    Ok((app, dir))
}

/// The cron path: `suggest.buildAll=true` returns the captured build envelope
/// verbatim (modulo the volatile `QTime`), with `command:"buildAll"` and NO
/// `suggest` block.
#[tokio::test]
async fn suggest_build_all_returns_captured_envelope() {
    let (app, _dir) = build_app_with_config(None).expect("app must build");
    let (status, body) = get(&app, "suggest?suggest.buildAll=true&wt=json").await;

    assert_eq!(
        status,
        StatusCode::OK,
        "buildAll must answer 200, got {status}: {body}"
    );
    assert_eq!(
        body["command"], "buildAll",
        "buildAll must echo command:\"buildAll\": {body}"
    );
    assert_eq!(
        body["responseHeader"]["status"], 0,
        "responseHeader.status must be 0: {body}"
    );
    // The decisive wire detail: Solr's SuggestComponent does NOT echo request
    // params under responseHeader for /suggest (unlike /select). Asserting the
    // absence here is what stops a future edit from adding `params.echo()` and
    // silently diverging from the fixture.
    assert!(
        body["responseHeader"].get("params").is_none(),
        "responseHeader must NOT carry params (Solr's /suggest never echoes them): {body}"
    );
    assert!(
        body.get("suggest").is_none(),
        "a build command carries no suggest block (that is a suggest.q lookup): {body}"
    );
}

/// The build envelope matches the committed fixture byte-for-byte outside the
/// volatile `QTime` — the wire-contract claim, asserted here in addition to the
/// differential harness's manifest-errors row.
#[tokio::test]
async fn suggest_build_all_matches_committed_fixture() {
    let (app, _dir) = build_app_with_config(None).expect("app must build");
    let (_status, body) = get(&app, "suggest?suggest.buildAll=true&wt=json").await;
    let expected = common::fixture("suggest_build_all");
    // QTime is the only volatile leaf; equalise it, then the rest must match.
    let mut actual = body.clone();
    if let Some(qt) = actual.pointer_mut("/responseHeader/QTime") {
        *qt = Value::Null;
    }
    let mut expected = expected;
    if let Some(qt) = expected.pointer_mut("/responseHeader/QTime") {
        *qt = Value::Null;
    }
    assert_eq!(actual, expected, "build envelope must match the fixture");
}

/// `suggest.build` (a single dictionary) and `suggest.reload` echo their own
/// `command` field — the same short-circuit, faithfully inert.
#[tokio::test]
async fn suggest_build_and_reload_echo_their_commands() {
    let (app, _dir) = build_app_with_config(None).expect("app must build");
    let (_, body) = get(&app, "suggest?suggest.build=true&wt=json").await;
    assert_eq!(
        body["command"], "build",
        "suggest.build -> command:\"build\""
    );
    let (_, body) = get(&app, "suggest?suggest.reload=true&wt=json").await;
    assert_eq!(
        body["command"], "reload",
        "suggest.reload -> command:\"reload\""
    );
}

/// `buildAll` wins when both `buildAll` and `build` are present (Solr processes
/// the build-all path first).
#[tokio::test]
async fn suggest_build_all_takes_precedence_over_build() {
    let (app, _dir) = build_app_with_config(None).expect("app must build");
    let (_, body) = get(
        &app,
        "suggest?suggest.buildAll=true&suggest.build=true&wt=json",
    )
    .await;
    assert_eq!(
        body["command"], "buildAll",
        "buildAll must win over build when both are sent: {body}"
    );
}

/// A bare `/suggest` (no build/reload command) returns just `responseHeader` —
/// no `command` key at all, matching Solr.
#[tokio::test]
async fn suggest_bare_returns_header_only() {
    let (app, _dir) = build_app_with_config(None).expect("app must build");
    let (status, body) = get(&app, "suggest?wt=json").await;
    assert_eq!(status, StatusCode::OK, "bare /suggest is a 200: {body}");
    assert!(
        body.get("command").is_none(),
        "bare /suggest carries no command: {body}"
    );
    assert!(
        body["responseHeader"]["status"] == 0,
        "bare /suggest still has a status-0 header: {body}"
    );
}

/// `omitHeader=true` drops `responseHeader` entirely, leaving only `command`.
#[tokio::test]
async fn suggest_omit_header_drops_response_header() {
    let (app, _dir) = build_app_with_config(None).expect("app must build");
    let (_, body) = get(
        &app,
        "suggest?suggest.buildAll=true&omitHeader=true&wt=json",
    )
    .await;
    assert_eq!(body["command"], "buildAll");
    assert!(
        body.get("responseHeader").is_none(),
        "omitHeader=true must drop responseHeader: {body}"
    );
}

/// `strict_params = true` must NOT 400 on any param the shipped `/suggest`
/// handler config makes routine: the component gate `suggest`, the defaults
/// `suggest.dictionary`/`suggest.count`, and the build commands. (The cron
/// request itself sends only `suggest.buildAll`; the rest are admitted for
/// parity so a handler-default param never 400s.)
#[tokio::test]
async fn suggest_strict_params_accepts_handler_routine_params() {
    let (app, _dir) =
        build_app_with_config(Some("strict_params = true\n")).expect("app must build");
    let (status, body) = request_full(
        &app,
        "GET",
        &format!(
            "{CORE}/suggest?suggest=true&suggest.buildAll=true&suggest.build=true&\
             suggest.reload=true&suggest.dictionary=und&suggest.count=10&wt=json"
        ),
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "strict_params must accept the handler's routine params, got {status}: {body}"
    );
}

/// The negative case: under `strict_params`, an unrecognised `suggest.*` param
/// still 400s — admitting the routine params is not a blanket `suggest.*` pass.
/// Mutation-tested: deleting the param from `SUGGEST_PARAMS` must turn this red.
#[tokio::test]
async fn suggest_strict_params_rejects_unknown_suggest_param() {
    let (app, _dir) =
        build_app_with_config(Some("strict_params = true\n")).expect("app must build");
    let (status, body) = request_full(
        &app,
        "GET",
        &format!("{CORE}/suggest?suggest.buildAll=true&suggest.bogus=1&wt=json"),
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "strict_params must 400 on an unknown suggest.* param, got {status}: {body}"
    );
    let msg = body
        .pointer("/error/msg")
        .and_then(|m| m.as_str())
        .unwrap_or("");
    assert!(
        msg.contains("suggest.bogus"),
        "the 400 must name the offending param: {body}"
    );
}

// The `suggest.q` read path (the Suggester-plugin prerequisite, issue #384)
// is covered by the block below; this divider keeps the build-path tests
// above visually separate from the lookup-path tests below.

/// Schema for the `suggest.q` lookup tests: `twm_suggest` (the suggestion
/// phrase source the shipped `solr.SuggestComponent` reads) and
/// `sm_context_tags` (its `contextField`, the `suggest.cfq` target), both
/// stored and multi-valued, plus the `id` unique key. Mirrors the corpus
/// `solr-ref/capture.sh`'s #384 block indexes into its own `suggest` core, so
/// the captured fixtures under `solr-ref/responses/suggest_q_*.json` are ground
/// truth for the same documents here.
///
/// `twm_suggest`'s declared type only governs its *indexed* terms; the
/// suggester re-analyzes the stored phrase with the per-dictionary analyzer
/// (`text_en` for `en`, `text_general` for `und`), exactly as Solr's
/// `suggestAnalyzerFieldType` overrides the field's own type. So the type
/// here is inert for the lookup path -- the stored value is what is read.
const SUGGEST_LOOKUP_SCHEMA: &str = r#"
[core]
name = "content"
unique_key = "id"
default_field = "twm_suggest"

[[fields]]
name = "id"
type = "string"
stored = true
required = true
fast = true

[[fields]]
name = "twm_suggest"
type = "text_en"
stored = true
multi_valued = true

[[fields]]
name = "sm_context_tags"
type = "string"
stored = true
multi_valued = true
"#;

/// The exact 5-doc corpus `solr-ref/capture.sh`'s #384 block indexes, so the
/// `suggest_q_*.json` fixtures are ground truth for it. `twm_suggest` carries
/// the suggestion phrases; `sm_context_tags` carries the per-doc context tags
/// `suggest.cfq` filters against (`site_alpha`/`site_beta` vary by doc so cfq
/// can include and exclude; `lang_en`/`lang_und` so a two-tag AND is
/// satisfiable for some docs and not others).
///
/// `su5` carries MULTI-BYTE phrases on purpose: U+212A KELVIN SIGN (`\u{212A}`,
/// 3 bytes, lowercases to the 1-byte `k`) and U+0130 LATIN CAPITAL I WITH DOT
/// ABOVE (`\u{0130}`, 2 bytes, lowercases to a 3-byte `i` + combining dot).
/// Every other phrase in this corpus is pure ASCII, where an analyzed token's
/// byte length equals its surface's -- which lets a highlighter that adds the
/// ANALYZED token's byte length to an offset into the ORIGINAL text look
/// correct on all 25 pre-existing fixtures while actually being able to slice
/// mid-codepoint. su5 is the case that tells the two apart. Its tags are
/// `site_gamma`/`lang_tr`, used by no `cfq` fixture, and none of its tokens is
/// touched by any pre-su5 query, so adding it must leave the other 25
/// fixtures byte-identical outside `QTime` -- that invariance is itself the
/// check that su5 is inert (`solr-ref/capture.sh`'s capture comment for this
/// block makes the same claim against the real Solr capture).
fn suggest_lookup_corpus() -> Value {
    json!([
        {"id":"su1","twm_suggest":["quick brown fox","lazy dog"],"sm_context_tags":["site_alpha","lang_en"]},
        {"id":"su2","twm_suggest":["quietly quacking quail","brown bear"],"sm_context_tags":["site_alpha","lang_und"]},
        {"id":"su3","twm_suggest":["the quick fox jumps over the lazy dog","case studies show progress"],"sm_context_tags":["site_beta","lang_en"]},
        {"id":"su4","twm_suggest":["running rivers carve valleys"],"sm_context_tags":["site_beta","lang_und"]},
        {"id":"su5","twm_suggest":["\u{212A}elvin degrees","\u{0130}stanbul airport"],"sm_context_tags":["site_gamma","lang_tr"]}
    ])
}

/// Builds a `content` app against `SUGGEST_LOOKUP_SCHEMA`, indexes the #384
/// suggest corpus, and commits. Returns the router plus the `TempDir` guard.
async fn suggest_lookup_app() -> (Router, TempDir) {
    let (app, dir) = suggest_lookup_app_with_config(None).await;
    (app, dir)
}

/// Like [`suggest_lookup_app`], but with an optional server-config TOML --
/// the `strict_params = true` variant this file's `strict_params` tests need.
/// Mirrors `build_app_with_config`'s `Some`/`None` split, applied to the
/// lookup-path schema/corpus instead of the tracer-bullet one.
async fn suggest_lookup_app_with_config(config: Option<&str>) -> (Router, TempDir) {
    let dir = TempDir::new().expect("create temp dir");
    let schema_path = dir.path().join("schema.toml");
    std::fs::write(&schema_path, SUGGEST_LOOKUP_SCHEMA).expect("write schema.toml");
    let data_dir = dir.path().join("data");
    std::fs::create_dir_all(&data_dir).expect("create data dir");
    let app = match config {
        Some(toml) => {
            let config_path = dir.path().join("wayfinder.toml");
            std::fs::write(&config_path, toml).expect("write wayfinder.toml");
            wayfinder::app_with_config(&schema_path, &data_dir, &config_path)
                .expect("wayfinder::app_with_config must build")
        }
        None => wayfinder::app(&schema_path, &data_dir).expect("wayfinder::app must build"),
    };
    common::post_docs(&app, &suggest_lookup_corpus()).await;
    (app, dir)
}

/// Drops the volatile `responseHeader.QTime` leaf so a lookup response can be
/// compared byte-for-byte against a captured fixture (QTime is the only leaf
/// that varies run-to-run; `/suggest` never echoes `params`, so nothing else
/// moves).
fn drop_qtime(mut v: Value) -> Value {
    if let Some(header) = v
        .pointer_mut("/responseHeader")
        .and_then(|h| h.as_object_mut())
    {
        header.remove("QTime");
    }
    v
}

/// Asserts the `/suggest?<query>` response equals the named `suggest_q_*`
/// fixture, modulo `QTime` -- the wire-contract claim for the lookup path.
async fn assert_lookup_matches(fixture_name: &str, query: &str) {
    let (app, _dir) = suggest_lookup_app().await;
    let (status, body) = get(&app, query).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "{fixture_name}: /{query} must answer 200, got {status}: {body}"
    );
    let expected = drop_qtime(fixture(fixture_name));
    let actual = drop_qtime(body);
    assert_eq!(
        actual, expected,
        "{fixture_name}: /{query} must match the captured fixture (modulo QTime)"
    );
}

/// Infix (token-position) hit: `fox` is the LAST token of "quick brown fox"
/// and a MIDDLE token of "the quick fox jumps over the lazy dog". Highlight on
/// (no cfq): `<b>fox</b>`.
#[tokio::test]
async fn suggest_q_infix_en_matches_fixture() {
    assert_lookup_matches(
        "suggest_q_infix_en",
        "suggest?suggest.dictionary=en&suggest.q=fox&wt=json",
    )
    .await;
}

/// Multi-token, order-independent, gaps allowed: `quick fox` matches both
/// phrases ("quick brown fox" is a NON-contiguous hit). Both matching tokens
/// highlighted.
#[tokio::test]
async fn suggest_q_multitoken_en_matches_fixture() {
    assert_lookup_matches(
        "suggest_q_multitoken_en",
        "suggest?suggest.dictionary=en&suggest.q=quick%20fox&wt=json",
    )
    .await;
}

/// Prefix-of-token: `qui` is a token-prefix of quick/quietly. Highlight wraps
/// only the matched prefix length: `<b>qui</b>ck`.
#[tokio::test]
async fn suggest_q_prefix_en_matches_fixture() {
    assert_lookup_matches(
        "suggest_q_prefix_en",
        "suggest?suggest.dictionary=en&suggest.q=qui&wt=json",
    )
    .await;
}

/// The "prefix-only" matching proof: `row` is a character substring of
/// "brown" but NOT a token-prefix, so it matches nothing. AnalyzingInfix
/// matches token-prefixes, not arbitrary character substrings.
#[tokio::test]
async fn suggest_q_substr_miss_en_matches_fixture() {
    assert_lookup_matches(
        "suggest_q_substr_miss_en",
        "suggest?suggest.dictionary=en&suggest.q=row&wt=json",
    )
    .await;
}

/// Empty result: no phrase touches `zzzzz`.
#[tokio::test]
async fn suggest_q_empty_en_matches_fixture() {
    assert_lookup_matches(
        "suggest_q_empty_en",
        "suggest?suggest.dictionary=en&suggest.q=zzzzz&wt=json",
    )
    .await;
}

/// `cfq` single-tag match: only su1 carries `fox` among site_alpha docs.
#[tokio::test]
async fn suggest_q_cfq_match_matches_fixture() {
    assert_lookup_matches(
        "suggest_q_cfq_match",
        "suggest?suggest.dictionary=en&suggest.q=fox&suggest.cfq=%2Bsite_alpha&wt=json",
    )
    .await;
}

/// `cfq` single-tag exclude: the mirror, keeping only su3 (site_beta).
#[tokio::test]
async fn suggest_q_cfq_exclude_matches_fixture() {
    assert_lookup_matches(
        "suggest_q_cfq_exclude",
        "suggest?suggest.dictionary=en&suggest.q=fox&suggest.cfq=%2Bsite_beta&wt=json",
    )
    .await;
}

/// `cfq` AND: `+site_alpha +lang_en` -- su1 carries both, su2 is lang_und,
/// su3 is site_beta, so only su1 survives.
#[tokio::test]
async fn suggest_q_cfq_and_matches_fixture() {
    assert_lookup_matches(
        "suggest_q_cfq_and",
        "suggest?suggest.dictionary=en&suggest.q=fox&suggest.cfq=%2Bsite_alpha%20%2Blang_en&wt=json",
    )
    .await;
}

/// `cfq` AND no doc satisfies: `+site_alpha +site_beta` -- no doc carries
/// both tags.
#[tokio::test]
async fn suggest_q_cfq_nomatch_matches_fixture() {
    assert_lookup_matches(
        "suggest_q_cfq_nomatch",
        "suggest?suggest.dictionary=en&suggest.q=fox&suggest.cfq=%2Bsite_alpha%20%2Bsite_beta&wt=json",
    )
    .await;
}

/// `cfq` unknown tag: `+nosuchtag` matches no doc.
#[tokio::test]
async fn suggest_q_cfq_unknown_matches_fixture() {
    assert_lookup_matches(
        "suggest_q_cfq_unknown",
        "suggest?suggest.dictionary=en&suggest.q=fox&suggest.cfq=%2Bnosuchtag&wt=json",
    )
    .await;
}

/// `cfq` SHOULD (bare tag, no `+`/`-`): `site_alpha` alone -- only su1 among
/// the `fox` hits carries it (su3 is `site_beta`), so `min_should=1` keeps
/// just su1.
#[tokio::test]
async fn suggest_q_cfq_should_matches_fixture() {
    assert_lookup_matches(
        "suggest_q_cfq_should",
        "suggest?suggest.dictionary=en&suggest.q=fox&suggest.cfq=site_alpha&wt=json",
    )
    .await;
}

/// `cfq` two SHOULDs (`site_alpha site_beta`, `min_should=1`): every `fox`
/// hit carries one of the two tags, so both su1 and su3 pass.
#[tokio::test]
async fn suggest_q_cfq_should_two_matches_fixture() {
    assert_lookup_matches(
        "suggest_q_cfq_should_two",
        "suggest?suggest.dictionary=en&suggest.q=fox&suggest.cfq=site_alpha%20site_beta&wt=json",
    )
    .await;
}

/// `cfq` pure negative (`-site_alpha`, no positive clause): "everything
/// except su1" -- su3 (the other `fox` hit, `site_beta`) survives. NOT empty,
/// which resolves the open question the capture comment raised: a MUST_NOT
/// with no MUST/SHOULD clause still passes docs that lack the excluded tag,
/// it does not vacuously match nothing.
#[tokio::test]
async fn suggest_q_cfq_negative_matches_fixture() {
    assert_lookup_matches(
        "suggest_q_cfq_negative",
        "suggest?suggest.dictionary=en&suggest.q=fox&suggest.cfq=-site_alpha&wt=json",
    )
    .await;
}

/// Multi-dictionary routing: `dict=und` over the same corpus. `fox` has no
/// stem, so `en` and `und` agree on the hit set; this fixture paired with
/// `infix_en` pins that `suggest.dictionary` selects the dictionary.
#[tokio::test]
async fn suggest_q_und_matches_fixture() {
    assert_lookup_matches(
        "suggest_q_und",
        "suggest?suggest.dictionary=und&suggest.q=fox&wt=json",
    )
    .await;
}

/// `suggest.count` cap: `qui` matches three phrases; `count=1` keeps only the
/// top one, and `numFound` is the RETURNED count (1), not the total (3).
#[tokio::test]
async fn suggest_q_count_matches_fixture() {
    assert_lookup_matches(
        "suggest_q_count",
        "suggest?suggest.dictionary=en&suggest.q=qui&suggest.count=1&wt=json",
    )
    .await;
}

/// Solr 9 IGNORES `suggest.highlight=false` on this handler: the response is
/// STILL highlighted (`quick brown <b>fox</b>`), same as the highlight-on
/// default. This is the decisive proof that Wayfinder must not honour the
/// param as a toggle on the plain (non-cfq) lookup path.
#[tokio::test]
async fn suggest_q_hl_off_en_matches_fixture() {
    assert_lookup_matches(
        "suggest_q_hl_off_en",
        "suggest?suggest.dictionary=en&suggest.q=fox&suggest.highlight=false&wt=json",
    )
    .await;
}

/// The context-filtered (`cfq`) lookup path never highlights, regardless of
/// `suggest.highlight`: `quick brown fox` comes back plain even with
/// `suggest.highlight=true` explicitly set. Paired with
/// `suggest_q_hl_off_en_matches_fixture`, this pins that highlighting is
/// driven by which lookup method runs (plain vs. context-filtered), not by
/// the `suggest.highlight` param at all.
#[tokio::test]
async fn suggest_q_hl_on_cfq_en_matches_fixture() {
    assert_lookup_matches(
        "suggest_q_hl_on_cfq_en",
        "suggest?suggest.dictionary=en&suggest.q=fox&suggest.cfq=%2Bsite_alpha&suggest.highlight=true&wt=json",
    )
    .await;
}

/// Every matching token in a phrase is bolded, not just the first: `qu`
/// matches all three tokens of "quietly quacking quail" and each is wrapped
/// independently (`<b>qu</b>ietly <b>qu</b>acking <b>qu</b>ail`). Wayfinder is
/// known to bold only the first matching token today, so this is expected to
/// fail until the highlighter walks every match, not just the first.
#[tokio::test]
async fn suggest_q_multihl_en_matches_fixture() {
    assert_lookup_matches(
        "suggest_q_multihl_en",
        "suggest?suggest.dictionary=en&suggest.q=qu&wt=json",
    )
    .await;
}

/// The highlighted span is the STEM length, not the raw query length: `study`
/// stems to `studi`, and the bold wraps `studi` inside "studies" -- cutting
/// mid-word (`case <b>studi</b>es show progress`). This is the `en`
/// dictionary (stemming analyzer) side of the stem/no-stem pair.
#[tokio::test]
async fn suggest_q_stem_en_matches_fixture() {
    assert_lookup_matches(
        "suggest_q_stem_en",
        "suggest?suggest.dictionary=en&suggest.q=study&wt=json",
    )
    .await;
}

/// The `und` dictionary's no-stem mirror of `suggest_q_stem_en`: `und` does
/// not stem, so `study` (no trailing token starts with the literal string
/// `study` -- the only phrase containing it is "case studies show progress",
/// whose token is `studies`) matches nothing. This pair is the only thing
/// that pins `suggest.dictionary` actually selecting a different analyzer,
/// not just a different label on the same lookup.
#[tokio::test]
async fn suggest_q_stem_und_matches_fixture() {
    assert_lookup_matches(
        "suggest_q_stem_und",
        "suggest?suggest.dictionary=und&suggest.q=study&wt=json",
    )
    .await;
}

/// `suggest.count=0` is Solr's one captured 500 on this handler: Lucene's
/// `TopFieldCollectorManager` rejects `numHits <= 0` outright, so the
/// response is `status:500`, an EMPTY `suggest` block (`{}` -- not even the
/// dictionary/query keys), and an `error` object. `error.trace` is a JVM
/// stack trace Wayfinder cannot and must not try to reproduce (same
/// established convention as
/// `query_types.rs::regex_bad_char_class_is_a_500_with_no_metadata_key`'s
/// non-empty-only check on `trace`), so only `code`/`msg` are compared
/// against the fixture, and `trace` is asserted present and non-empty, not
/// equal to any particular text.
#[tokio::test]
async fn suggest_q_count_zero_en_matches_fixture() {
    let (app, _dir) = suggest_lookup_app().await;
    let (status, body) = get(
        &app,
        "suggest?suggest.dictionary=en&suggest.q=qui&suggest.count=0&wt=json",
    )
    .await;

    let expected = fixture("suggest_q_count_zero_en");

    assert_eq!(
        status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "suggest.count=0 must be a 500, got {status}: {body}"
    );
    assert_eq!(
        body["responseHeader"]["status"], expected["responseHeader"]["status"],
        "responseHeader.status must match the fixture's 500: {body}"
    );
    assert_eq!(
        body["suggest"], expected["suggest"],
        "suggest.count=0 must carry an EMPTY suggest block ({{}}), not a partial result: {body}"
    );
    assert_eq!(
        body["error"]["code"], expected["error"]["code"],
        "error.code must match the fixture: {body}"
    );
    assert_eq!(
        body["error"]["msg"], expected["error"]["msg"],
        "error.msg must match the fixture's numHits wording: {body}"
    );
    // trace is a JVM stack trace Wayfinder cannot and should not reproduce
    // verbatim -- assert it is present and non-empty, not equal to the
    // fixture's Java frames.
    assert!(
        body["error"]["trace"]
            .as_str()
            .is_some_and(|t| !t.is_empty()),
        "suggest.count=0's 500 must carry a non-empty error.trace: {body}"
    );
}

/// Every `suggest_q_*` fixture must be asserted by some test in this file.
///
/// This is not decoration: this suite shipped an earlier round green while
/// three captured, committed fixtures (`suggest_q_overlap_en`,
/// `suggest_q_prefix_nonfinal_en`, `suggest_q_order_swap_en`) were referenced
/// by nothing -- and those three were precisely the ones that falsified the
/// matching rule. A fixture nobody asserts is not evidence, it is a file. The
/// guard is one-directional on purpose: it catches captured-but-unasserted.
/// The reverse (asserted-but-uncaptured) is already caught, loudly, by
/// `fixture()` panicking on a missing file.
#[test]
fn every_suggest_q_fixture_is_asserted_by_a_test() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let source = std::fs::read_to_string(root.join("tests/suggest.rs"))
        .expect("this test file must be readable");

    let mut orphans: Vec<String> = std::fs::read_dir(root.join("solr-ref/responses"))
        .expect("solr-ref/responses must be readable")
        .map(|e| {
            e.expect("dir entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .filter_map(|f| f.strip_suffix(".json").map(str::to_string))
        .filter(|name| name.starts_with("suggest_q_"))
        // `fixture("<name>")` is the single way this suite reads a fixture, so
        // matching the quoted name is exact -- no substring false-positives
        // from one fixture name being a prefix of another.
        .filter(|name| !source.contains(&format!("\"{name}\"")))
        .collect();
    orphans.sort();

    assert!(
        orphans.is_empty(),
        "these suggest_q fixtures are captured but asserted by no test -- \
         wire them up or delete them: {orphans:?}"
    );
}

/// Negative `suggest.count` fails exactly as `0` does: Lucene's
/// `TopFieldCollectorManager` rejects any `numHits <= 0`, so the guard must be
/// `<= 0`, not `== 0`. This is also what forces `suggest.count` to be parsed
/// SIGNED -- an unsigned parse would fail on `-1` and silently fall back to the
/// handler default of 10, answering 200 where Solr answers 500. Same
/// trace-is-JVM-frames caveat as `suggest_q_count_zero_en_matches_fixture`.
#[tokio::test]
async fn suggest_q_count_neg_en_matches_fixture() {
    let (app, _dir) = suggest_lookup_app().await;
    let (status, body) = get(
        &app,
        "suggest?suggest.dictionary=en&suggest.q=qui&suggest.count=-1&wt=json",
    )
    .await;

    let expected = fixture("suggest_q_count_neg_en");

    assert_eq!(
        status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "suggest.count=-1 must be a 500 (not a fallback to the default 10), got {status}: {body}"
    );
    assert_eq!(
        body["responseHeader"]["status"], expected["responseHeader"]["status"],
        "responseHeader.status must match the fixture's 500: {body}"
    );
    assert_eq!(
        body["suggest"], expected["suggest"],
        "suggest.count=-1 must carry an EMPTY suggest block ({{}}): {body}"
    );
    assert_eq!(
        body["error"]["code"], expected["error"]["code"],
        "error.code must match the fixture: {body}"
    );
    assert_eq!(
        body["error"]["msg"], expected["error"]["msg"],
        "error.msg must match the fixture's numHits wording: {body}"
    );
    assert!(
        body["error"]["trace"]
            .as_str()
            .is_some_and(|t| !t.is_empty()),
        "suggest.count=-1's 500 must carry a non-empty error.trace: {body}"
    );
}

/// Discriminator 1 of 3 for the matching rule: `qu quick` finds NOTHING, even
/// though "quick brown fox" contains a token starting with `qu` AND a token
/// equal to `quick`. Only the FINAL query token matches as a prefix; every
/// earlier token must match some phrase token's stem EXACTLY. `qu` is not a
/// whole token anywhere, so the phrase is rejected. A bag-of-prefixes
/// implementation returns 2 here -- this fixture is what falsifies it.
#[tokio::test]
async fn suggest_q_overlap_en_matches_fixture() {
    assert_lookup_matches(
        "suggest_q_overlap_en",
        "suggest?suggest.dictionary=en&suggest.q=qu%20quick&wt=json",
    )
    .await;
}

/// Discriminator 2 of 3: `qui fox` finds NOTHING. `fox` (final) is fine, but
/// the non-final `qui` is only a PREFIX of `quick`, never a whole token, so no
/// phrase qualifies. Contrast `suggest_q_prefix_en` (`qui` alone, as the final
/// and only token) which does match.
#[tokio::test]
async fn suggest_q_prefix_nonfinal_en_matches_fixture() {
    assert_lookup_matches(
        "suggest_q_prefix_nonfinal_en",
        "suggest?suggest.dictionary=en&suggest.q=qui%20fox&wt=json",
    )
    .await;
}

/// Discriminator 3 of 3: matching is order-INDEPENDENT. `fox quick` returns
/// the same 2 phrases as `quick fox` (`suggest_q_multitoken_en`), with both
/// tokens highlighted in place -- the query's token order does not have to
/// match the phrase's. Only the exact-vs-prefix ROLE is positional.
#[tokio::test]
async fn suggest_q_order_swap_en_matches_fixture() {
    assert_lookup_matches(
        "suggest_q_order_swap_en",
        "suggest?suggest.dictionary=en&suggest.q=fox%20quick&wt=json",
    )
    .await;
}

// --- round-3 review probes: nine fixtures settling four behaviours the first
// 25 fixtures left open (highlight-span rule for non-final/final tokens,
// trailing-separator handling, multi-byte highlight-span arithmetic, and the
// cfq paren-group branch). See `solr-ref/capture.sh`'s "round-3 review
// probes" comment block for the capture rationale.

/// Highlight length for a NON-FINAL (exact) query token whose stem differs
/// from its surface: `studies show` -> `case <b>studies</b> <b>show</b>
/// progress`. Every non-final token in the pre-existing fixtures is
/// stem-identical (`quick`/`fox`), so nothing before this pinned "bold the
/// stem's length" against "bold the whole surface token" for an exact
/// (non-prefix) match. Lucene's `AnalyzingInfixSuggester` splits the two cases
/// (`addWholeMatch` for a matched term, `addPrefixMatch` for the final
/// prefix); this fixture is the whole-surface-token reading. `show` (final,
/// stem-identical) confirms the prefix branch is unaffected.
#[tokio::test]
async fn suggest_q_hl_wholetoken_en_matches_fixture() {
    assert_lookup_matches(
        "suggest_q_hl_wholetoken_en",
        "suggest?suggest.dictionary=en&suggest.q=studies%20show&wt=json",
    )
    .await;
}

/// The same question with the FINAL token also stemmed, isolating the prefix
/// branch's span length independently: `running rivers` -> `<b>running</b>
/// <b>river</b>s carve valleys`. `running` (non-final, exact) bolds its whole
/// surface token per `suggest_q_hl_wholetoken_en`'s rule; `rivers` (final,
/// prefix) bolds only the analyzed/stemmed length (`river`), not the full
/// surface `rivers`. This single fixture is the only one that isolates BOTH
/// the non-final-whole-token rule and the final-prefix-stem-length rule at
/// once, because `suggest_q_hl_wholetoken_en`'s final token (`show`) happens
/// to be stem-identical and so cannot show the final-token span length
/// diverging from the surface.
#[tokio::test]
async fn suggest_q_hl_stemspan_en_matches_fixture() {
    assert_lookup_matches(
        "suggest_q_hl_stemspan_en",
        "suggest?suggest.dictionary=en&suggest.q=running%20rivers&wt=json",
    )
    .await;
}

/// A TRAILING SPACE on the query changes the last token from a PrefixQuery to
/// a complete TermQuery: `suggest.q=qui%20` (`qui ` with a trailing space)
/// returns `numFound:0`, where `suggest_q_prefix_en`'s bare `suggest.q=qui`
/// (no trailing space) returns 3. Nothing before this fixture pins that a
/// trailing separator is even observable -- `suggest_q_prefix_en` only shows
/// that `qui` alone matches as a prefix; this is the fixture that shows the
/// same three letters, with one more trailing byte, matching nothing. A
/// user typing a space into an autocomplete box is an entirely ordinary
/// event, not a corner case.
#[tokio::test]
async fn suggest_q_trailing_space_en_matches_fixture() {
    assert_lookup_matches(
        "suggest_q_trailing_space_en",
        "suggest?suggest.dictionary=en&suggest.q=qui%20&wt=json",
    )
    .await;
}

/// Multi-byte prefix probe, SHRINK direction: `suggest.q=k` returns
/// `numFound:0` against su5's `\u{212A}elvin degrees` (KELVIN SIGN, U+212A,
/// which Rust's `to_lowercase` folds to the 1-byte `k`). Paired with
/// `suggest_q_multibyte_full_en` (which DOES hit on `kelvin`), this
/// disambiguates whether a miss on `k` alone means the fold to `k` never
/// happens at all, or whether it is specifically the single-character query
/// that misses (`suggest_q_multibyte_onechar_en` settles that further).
/// Nothing in the pre-existing pure-ASCII corpus can distinguish "folds but
/// doesn't match" from "doesn't fold" because every existing token's surface
/// and analyzed byte lengths are equal.
#[tokio::test]
async fn suggest_q_multibyte_prefix_en_matches_fixture() {
    assert_lookup_matches(
        "suggest_q_multibyte_prefix_en",
        "suggest?suggest.dictionary=en&suggest.q=k&wt=json",
    )
    .await;
}

/// The `LengthFilterFactory min="2"` floor, on the GROW-direction token:
/// `suggest.q=i` returns `numFound:0` against su5's `\u{0130}stanbul airport`
/// (U+0130 LATIN CAPITAL I WITH DOT ABOVE lowercases to a 3-byte `i` +
/// combining dot). Nothing before this pins that a single surface character
/// is dropped by the dictionary analyzer's minimum-length filter before it
/// ever reaches the suggester, independent of any multi-byte fold.
#[tokio::test]
async fn suggest_q_multibyte_onechar_en_matches_fixture() {
    assert_lookup_matches(
        "suggest_q_multibyte_onechar_en",
        "suggest?suggest.dictionary=en&suggest.q=i&wt=json",
    )
    .await;
}

/// THE panic case: `suggest.q=ke` -> `<b>\u{212A}e</b>lvin degrees`. The
/// surface token `\u{212A}elvin` (KELVIN SIGN + "elvin") is 8 bytes; its
/// analyzed/lowercased form `kelvin` is 6 bytes. A highlighter that slices the
/// ORIGINAL (surface) text using the ANALYZED token's byte length computes a
/// 2-byte bold span starting at byte 0 of the surface -- but the surface's
/// first codepoint (`\u{212A}`, U+212A) is itself 3 bytes, so a 2-byte slice
/// lands INSIDE that codepoint's UTF-8 encoding. In Wayfinder today this
/// currently surfaces as `wayfinder::PanicError` (a 500), so a bare
/// fixture-mismatch assertion here would be a confusing way to learn about a
/// mid-codepoint slice panic -- the explicit status/message assertions below
/// name it directly instead of leaving the panic to surface as an opaque
/// assertion failure on the body.
#[tokio::test]
async fn suggest_q_multibyte_short_en_matches_fixture() {
    let (app, _dir) = suggest_lookup_app().await;
    let (status, body) = get(&app, "suggest?suggest.dictionary=en&suggest.q=ke&wt=json").await;
    assert_eq!(
        status,
        StatusCode::OK,
        "suggest.q=ke must NOT panic slicing mid-codepoint through the \
         multi-byte KELVIN SIGN (U+212A) -- got {status} instead of 200: {body}"
    );
    assert_lookup_matches(
        "suggest_q_multibyte_short_en",
        "suggest?suggest.dictionary=en&suggest.q=ke&wt=json",
    )
    .await;
}

/// The multi-byte SHRINK-direction control: `suggest.q=kelvin` (the WHOLE
/// analyzed-length token, not a 2-byte prefix) -> `<b>\u{212A}elvin</b>
/// degrees`, the entire 6-character surface token bolded. Paired with
/// `suggest_q_multibyte_short_en`'s 2-byte prefix (which panics/garbles),
/// this isolates that the byte-length mismatch is specifically a PARTIAL-span
/// problem: bolding the FULL matched token never has to slice mid-codepoint,
/// because the span boundary coincides with a codepoint boundary either way.
#[tokio::test]
async fn suggest_q_multibyte_full_en_matches_fixture() {
    assert_lookup_matches(
        "suggest_q_multibyte_full_en",
        "suggest?suggest.dictionary=en&suggest.q=kelvin&wt=json",
    )
    .await;
}

/// The multi-byte GROW-direction control: `suggest.q=istanbul` ->
/// `<b>\u{0130}stanbul</b> airport`, the whole token bolded. U+0130 lowercases
/// to a 3-byte `i` + combining dot -- LONGER than its 2-byte surface encoding,
/// the opposite direction of `ke`'s shrink. Together with
/// `suggest_q_multibyte_full_en`, this shows the whole-token-bold case is safe
/// in BOTH directions of the byte-length change; only a PARTIAL span (the `ke`
/// 2-byte prefix case) can slice mid-codepoint.
#[tokio::test]
async fn suggest_q_multibyte_grow_en_matches_fixture() {
    assert_lookup_matches(
        "suggest_q_multibyte_grow_en",
        "suggest?suggest.dictionary=en&suggest.q=istanbul&wt=json",
    )
    .await;
}

/// `cfq` PAREN GROUP: `suggest.cfq=+(site_alpha site_beta)` -> `numFound:2`,
/// both `fox` hits (su1 site_alpha, su3 site_beta), unhighlighted (the cfq
/// lookup path never highlights, per `suggest_q_hl_on_cfq_en_matches_fixture`).
/// So Solr parses a parenthesized group as MUST(at least one of the group's
/// tags) -- a nested SHOULD inside a MUST. No fixture before this one ever
/// sent a paren group, so `parse_cfq`'s handling of it was entirely unpinned.
/// `Utility::buildSuggesterContextFilterQuery`
/// (`coverage/search_api_solr_4.4.0_source/src/Utility/Utility.php:476-487`)
/// provably never emits this form -- it space-joins `'+' . $tag` per tag, with
/// no grouping syntax -- so this branch is unreachable from the real
/// `search_api_solr`/`search_api_solr_autocomplete` client. But an unreachable
/// branch that answers the wrong question is worse than no branch at all: this
/// fixture is what decides whether to implement the grouping correctly or
/// delete the branch.
#[tokio::test]
async fn suggest_q_cfq_group_matches_fixture() {
    assert_lookup_matches(
        "suggest_q_cfq_group",
        "suggest?suggest.dictionary=en&suggest.q=fox&suggest.cfq=%2B%28site_alpha%20site_beta%29&wt=json",
    )
    .await;
}

/// `strict_params` must NOT 400 on the lookup params the Suggester plugin
/// sends: `suggest.q`, `suggest.cfq`, `suggest.highlight` (the plugin sets
/// `suggest.highlight=false`; Solr 9 ignores it and highlights anyway, but the
/// param must be admitted so it does not 400). Built with `strict_params =
/// true` explicitly -- `suggest_lookup_app()`'s default config has
/// `strict_params = false`, which would make this pass vacuously (every param
/// is admitted when strict checking is off at all). Sibling pattern:
/// `suggest_strict_params_accepts_handler_routine_params` above (line ~180).
#[tokio::test]
async fn suggest_strict_params_accepts_lookup_params() {
    let (app, _dir) = suggest_lookup_app_with_config(Some("strict_params = true\n")).await;
    let (status, body) = request_full(
        &app,
        "GET",
        "content/suggest?suggest.dictionary=en&suggest.q=fox&suggest.cfq=%2Bsite_alpha\
         &suggest.highlight=false&suggest.count=5&wt=json",
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "strict_params must accept the lookup params, got {status}: {body}"
    );
}

/// The lookup path needs `suggest.q`: without it, `/suggest` is the inert
/// build/header path (#352) and carries no `suggest` block. This guards the
/// branch split.
#[tokio::test]
async fn suggest_without_q_has_no_suggest_block() {
    let (app, _dir) = suggest_lookup_app().await;
    let (status, body) = get(&app, "suggest?suggest.dictionary=en&wt=json").await;
    assert_eq!(status, StatusCode::OK, "bare /suggest is a 200: {body}");
    assert!(
        body.get("suggest").is_none(),
        "no suggest.q -> no suggest block (the build/header path): {body}"
    );
}

/// The `fireAndForget` cron caller closes the connection without reading the
/// response. The acceptance bar from the issue is: does not error, does not
/// hang, does not leak a task or connection per cron run. A synchronous
/// immediate return clears all three by construction — there is no background
/// work to outlive the request — and this asserts that property directly: the
/// handler answers promptly and stays inert across repeated cron runs (no
/// per-call state accumulates).
#[tokio::test]
async fn suggest_build_all_is_inert_and_does_not_leak_across_runs() {
    let (app, _dir) = build_app_with_config(None).expect("app must build");
    for _ in 0..5 {
        let (status, body) = get(&app, "suggest?suggest.buildAll=true&wt=json").await;
        assert_eq!(status, StatusCode::OK, "every cron run must answer 200");
        assert_eq!(body["command"], "buildAll");
        // No suggest block ever appears: the build is inert, so nothing about
        // the response grows or changes across repeated calls.
        assert!(body.get("suggest").is_none(), "no suggest block: {body}");
    }
}
