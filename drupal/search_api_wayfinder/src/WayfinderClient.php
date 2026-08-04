<?php

declare(strict_types=1);

namespace Drupal\search_api_wayfinder;

use Drupal\search_api\SearchApiException;
use GuzzleHttp\ClientInterface;
use GuzzleHttp\Exception\GuzzleException;
use GuzzleHttp\Exception\RequestException;

/**
 * Thin Guzzle wrapper for Wayfinder's Solr-wire-compatible core endpoints:
 * select, mlt, update, admin/system, and admin/ping.
 *
 * Converts non-200 Solr error envelopes ({"error":{"msg":..., "code":...}})
 * into SearchApiException using the envelope's error.msg, per plan doc
 * "Architecture" section.
 */
class WayfinderClient {

  public function __construct(
    private readonly ClientInterface $httpClient,
    private readonly string $coreUrl,
    private readonly ?float $timeout = NULL,
    private readonly string $username = '',
    private readonly string $password = '',
  ) {}

  /**
   * GET {core}/select with the given params.
   *
   * @return array
   *   The decoded JSON response body.
   *
   * @throws \Drupal\search_api\SearchApiException
   */
  public function select(array $params): array {
    $params['wt'] = 'json';
    return $this->request('GET', 'select', ['query' => $this->encodeQuery($params)]);
  }

  /**
   * GET {core}/mlt with the given params.
   *
   * Same transport and error handling as select(); only the endpoint differs.
   * The success envelope carries both a "match" block (the seed document) and
   * a "response" block (the similar documents) -- ground truth
   * solr-ref/responses/mlt_baseline.json.
   *
   * @return array
   *   The decoded JSON response body.
   *
   * @throws \Drupal\search_api\SearchApiException
   */
  public function mlt(array $params): array {
    $params['wt'] = 'json';
    return $this->request('GET', 'mlt', ['query' => $this->encodeQuery($params)]);
  }

  /**
   * GET {core}/terms with the given params.
   *
   * The autocomplete transport (#291): search_api_autocomplete's stock Server
   * suggester reads the indexed term dictionary through Solr's Terms component
   * (finding 154 -- the SuggestComponent is not on any evidenced client path),
   * so the backend's getAutocompleteSuggestions() GETs /terms with
   * terms=true, terms.fl, terms.prefix, terms.limit. Same transport and error
   * handling as select()/mlt(); only the endpoint differs. The success
   * envelope is {terms: {<field>: [<term>, <count>, ...]}} -- ground truth
   * solr-ref/responses/terms_prefix_body_multi.json.
   *
   * @return array
   *   The decoded JSON response body.
   *
   * @throws \Drupal\search_api\SearchApiException
   */
  public function terms(array $params): array {
    $params['wt'] = 'json';
    return $this->request('GET', 'terms', ['query' => $this->encodeQuery($params)]);
  }

  /**
   * POST {core}/update with the given command body.
   *
   * @param array $command
   *   The update command body (e.g. ["add" => ["doc" => [...]]]).
   * @param array $queryParams
   *   Extra /update query params, e.g. ['commitWithin' => 1000]. Wayfinder's
   *   parse_update_commands only reads the "add"/"delete" body -- commitWithin
   *   is read from the query string (UPDATE_PARAMS), not the body.
   *
   * @return array
   *   The decoded JSON response body.
   *
   * @throws \Drupal\search_api\SearchApiException
   */
  public function update(array $command, array $queryParams = []): array {
    $queryParams['wt'] = 'json';
    return $this->request('POST', 'update', [
      'query' => $this->encodeQuery($queryParams),
      'json' => $command,
    ]);
  }

  /**
   * GET {core}/admin/system.
   *
   * Same transport and error handling as select(); only the endpoint differs.
   * The success envelope carries the version handshake under
   * lucene.wayfinder-spec-version -- ground truth
   * solr-ref/responses/admin_system.json.
   *
   * @return array
   *   The decoded JSON response body.
   *
   * @throws \Drupal\search_api\SearchApiException
   */
  public function adminSystem(): array {
    return $this->request('GET', 'admin/system', ['query' => $this->encodeQuery(['wt' => 'json'])]);
  }

  /**
   * GET {core}/admin/ping.
   *
   * Never throws: connection errors and non-200 responses both yield FALSE.
   */
  public function ping(): bool {
    try {
      $options = [
        'query' => $this->encodeQuery(['wt' => 'json']),
      ];
      $options += $this->authenticationOptions();
      $response = $this->httpClient->request('GET', $this->coreUrl . '/admin/ping', $options);
      return $response->getStatusCode() === 200;
    }
    catch (GuzzleException $e) {
      return FALSE;
    }
  }

  /**
   * POST {core}/update/extract -- multipart file upload, extractOnly=true.
   *
   * The tracer transport for issue #262's file-extraction slice (endpoint
   * landed in #258). Mirrors the wire shape capture.sh's `cap_extract` and
   * Solarium's ExtractQuery produce: a single multipart part named "file"
   * carrying the upload, with `extractOnly=true`, `extractFormat=text`, and
   * `resource.name=<basename>` on the query string. The default `extractFormat`
   * is XHTML; we ask for plain text because the result is fulltext index
   * content, not a rendered document.
   *
   * The returned array is the full decoded envelope; the extracted text lives
   * under the "file" key (the part name), NOT `resource.name` -- #258 finding:
   * Wayfinder emits the raw key and the client reads it directly, with no
   * Solarium-style client-side aliasing. WayfinderBackend::extractContentFromFile()
   * is where that key is pulled out.
   *
   * @param string $filepath
   *   Real filesystem path to the file to upload. The file is streamed, not
   *   buffered into memory.
   *
   * @return array
   *   The decoded JSON response body (e.g. {responseHeader, file,
   *   file_metadata}).
   *
   * @throws \Drupal\search_api\SearchApiException
   *   On a non-200 error envelope, a transport failure, or an unreadable file.
   */
  public function extract(string $filepath): array {
    $filename = basename($filepath);
    // Stream the file rather than reading it into memory: attachments can be
    // large, and #257's request-byte budget lives on the server side. fopen
    // failure (missing/unreadable file) becomes a SearchApiException so the
    // processor's per-file catch can log-and-continue instead of leaking a
    // PHP warning.
    $stream = @fopen($filepath, 'r');
    if ($stream === FALSE) {
      throw new SearchApiException(sprintf('Unable to read file for extraction: %s', $filepath));
    }

    return $this->request('POST', 'update/extract', [
      'query' => $this->encodeQuery([
        'extractOnly' => 'true',
        'extractFormat' => 'text',
        'resource.name' => $filename,
        'wt' => 'json',
      ]),
      'multipart' => [
        [
          'name' => 'file',
          'filename' => $filename,
          'contents' => $stream,
        ],
      ],
    ]);
  }

  /**
   * Encodes Solr query parameters without PHP's bracket notation for repeated
   * values. Guzzle's array encoder turns fq values into fq[0], fq[1], which
   * is not Solr-wire-compatible.
   *
   * Any array-valued param is emitted as a repeated key (fq, facet.field,
   * ...): that is the Solr wire convention for every multi-valued param, so
   * this is general rather than a per-param allow-list. Nested arrays and
   * objects are still rejected as non-scalar.
   */
  private function encodeQuery(array $params): string {
    $encoded = [];
    foreach ($params as $name => $value) {
      $values = is_array($value) ? $value : [$value];
      foreach ($values as $item) {
        if (!is_scalar($item) && $item !== NULL) {
          throw new \InvalidArgumentException('Query parameters must be scalar values.');
        }
        $encoded[] = rawurlencode((string) $name) . '=' . rawurlencode((string) $item);
      }
    }
    return implode('&', $encoded);
  }

  /**
   * Returns Guzzle's HTTP Basic option only for a complete credential pair.
   *
   * @return array<string, array{0: string, 1: string}>
   */
  private function authenticationOptions(): array {
    return $this->username !== '' && $this->password !== ''
      ? ['auth' => [$this->username, $this->password]]
      : [];
  }

  /**
   * Performs a request against a core-relative endpoint and decodes the JSON
   * body, converting non-200 error envelopes into SearchApiException.
   */
  private function request(string $method, string $endpoint, array $options): array {
    if ($this->timeout !== NULL) {
      $options += ['timeout' => $this->timeout, 'connect_timeout' => $this->timeout];
    }
    $options += $this->authenticationOptions();

    try {
      $response = $this->httpClient->request($method, $this->coreUrl . '/' . $endpoint, $options);
      $decoded = json_decode((string) $response->getBody(), TRUE);
      return is_array($decoded) ? $decoded : [];
    }
    catch (RequestException $e) {
      $response = $e->getResponse();
      if ($response) {
        $body = json_decode((string) $response->getBody(), TRUE);
        $message = $body['error']['msg'] ?? $e->getMessage();
        throw new SearchApiException($message, 0, $e);
      }
      throw new SearchApiException($e->getMessage(), 0, $e);
    }
    catch (GuzzleException $e) {
      // Covers ConnectException (DNS failure, refused connection, timeout)
      // and any other non-RequestException transport failure: these have no
      // response to extract an error envelope from.
      throw new SearchApiException($e->getMessage(), 0, $e);
    }
  }

}
