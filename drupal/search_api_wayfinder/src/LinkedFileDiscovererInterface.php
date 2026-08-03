<?php

declare(strict_types=1);

namespace Drupal\search_api_wayfinder;

use Drupal\file\FileInterface;

/**
 * Resolves the files a piece of content *links to* (issue #265), as distinct
 * from the files an entity *attaches* via a file/image field (#262).
 *
 * The gap that motivated porting search_api_attachments into
 * search_api_wayfinder at all: a link to a downloadable document can appear
 * anywhere in a node -- body text, a link field, a paragraph, a Layout Builder
 * block -- and its extracted text should be indexed as part of that node.
 * search_api_attachments only handles files in file/image fields.
 *
 * Discovery is a pure function of markup/URIs plus Drupal's entity stores, so
 * it lives behind this interface and is unit-tested per mechanism in isolation
 * from the Search API processor that drives it. The processor (#265) reads
 * configured text and link fields, hands their raw values here, and indexes
 * whatever files come back.
 *
 * Mechanisms, in reliability order (from the issue):
 *  1. data-entity-type / data-entity-uuid attributes -- CKEditor writes these
 *     when a file or media is inserted; resolves to the entity directly with no
 *     URL parsing.
 *  2. <drupal-media> embeds (media_embed filter) -- same attribute shape as 1
 *     with data-entity-type="media"; the media's source file is resolved by
 *     inspecting its file-typed fields (no MediaInterface dependency, so the
 *     optional media module need not be enabled and the test sandbox needs no
 *     media shim).
 *  3. Link-field URIs -- entity:file/N and entity:media/N resolve directly;
 *     internal:/... and absolute URLs reduce to (4).
 *  4. Href parsing, last resort -- /sites/default/files/... (public) and
 *     /system/files/... (private) are reconstructed to their stream URI
 *     (public://, private://) via the stream wrapper manager and looked up in
 *     file_managed. External URLs never resolve to a managed file and so are
 *     excluded by construction -- nothing is fetched, so there is no SSRF.
 *
 * Parsing uses Html::load(), never regex. Depth is capped at one hop: the
 * discovered file's own content is never parsed for further links.
 */
interface LinkedFileDiscovererInterface {

  /**
   * Resolves the files linked or embedded in a chunk of HTML.
   *
   * Covers mechanisms 1, 2, and 4 (attribute embeds, <drupal-media>, hrefs).
   *
   * @param string $html
   *   Raw HTML, typically the value of a configured text field. Parsed with
   *   Html::load(); malformed markup is tolerated, never fatal.
   *
   * @return \Drupal\file\FileInterface[]
   *   The distinct files referenced, keyed by file id.
   */
  public function discoverFromHtml(string $html): array;

  /**
   * Resolves the file a link-field URI points at, if any.
   *
   * Covers mechanism 3 (entity:file/N, entity:media/N) and reduces internal:
   * and absolute URIs to the href mechanism (4). A URI that does not point at
   * a managed file (a node route, an external site) resolves to nothing.
   *
   * @param string $uri
   *   The link-field uri value (e.g. entity:file/2, internal:/foo, absolute).
   *
   * @return \Drupal\file\FileInterface[]
   *   Zero or one file, keyed by file id.
   */
  public function discoverFromLinkUri(string $uri): array;

}
