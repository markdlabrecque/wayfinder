<?php

declare(strict_types=1);

namespace Drupal\Tests\search_api_wayfinder\Unit;

use Drupal\search_api\SearchApiException;
use Drupal\search_api_wayfinder\WayfinderClient;
use GuzzleHttp\Client;
use GuzzleHttp\Handler\MockHandler;
use GuzzleHttp\HandlerStack;
use GuzzleHttp\Psr7\Response;
use PHPUnit\Framework\TestCase;

/**
 * Tests WayfinderClient: thin Guzzle wrapper for select()/update(), and its
 * conversion of Wayfinder's Solr-compatible error envelope into
 * SearchApiException.
 *
 * Error envelope shape ({"responseHeader":{"status":400,...},
 * "error":{"msg":"...","code":400}}) is ground truth from
 * solr-ref/responses/err_bad_sort.json. Success envelope shapes for /select
 * and /update are from solr-ref/responses/err_missing_q.json (200,
 * response.numFound/docs) and solr-ref/responses/update_add_commit.json
 * (200, no "response" key, just responseHeader) respectively.
 *
 * @coversDefaultClass \Drupal\search_api_wayfinder\WayfinderClient
 * @group search_api_wayfinder
 */
class WayfinderClientTest extends TestCase {

  private function clientWithResponses(array $responses, string $username = '', string $password = ''): WayfinderClient {
    $mock = new MockHandler($responses);
    $handlerStack = HandlerStack::create($mock);
    $httpClient = new Client(['handler' => $handlerStack]);
    return new WayfinderClient($httpClient, 'http://localhost:8983/solr/mycore', NULL, $username, $password);
  }

  /**
   * Builds a client with request history so authentication headers can be
   * asserted on the actual outgoing PSR-7 requests.
   *
   * @param array<int, array<string, mixed>> $history
   */
  private function clientWithHistory(array $responses, array &$history, ?string $username = NULL, ?string $password = NULL): WayfinderClient {
    $history = [];
    $mock = new MockHandler($responses);
    $handlerStack = HandlerStack::create($mock);
    $handlerStack->push(\GuzzleHttp\Middleware::history($history));
    $httpClient = new Client(['handler' => $handlerStack]);

    return $username === NULL && $password === NULL
      ? new WayfinderClient($httpClient, 'http://localhost:8983/solr/mycore')
      : new WayfinderClient($httpClient, 'http://localhost:8983/solr/mycore', NULL, $username ?? '', $password ?? '');
  }

  /**
   * @covers ::select
   */
  public function testSelectReturnsDecodedBodyOn200(): void {
    $body = (string) file_get_contents(__DIR__ . '/../../../../../solr-ref/responses/err_missing_q.json');
    $client = $this->clientWithResponses([new Response(200, [], $body)]);

    $result = $client->select(['q' => '*:*']);

    $this->assertSame(0, $result['response']['numFound']);
  }

  /**
   * @covers ::update
   */
  public function testUpdateReturnsDecodedBodyOn200(): void {
    $body = (string) file_get_contents(__DIR__ . '/../../../../../solr-ref/responses/update_add_commit.json');
    $client = $this->clientWithResponses([new Response(200, [], $body)]);

    $result = $client->update(['add' => ['doc' => ['id' => 'x']]]);

    $this->assertSame(0, $result['responseHeader']['status']);
  }

  /**
   * @covers ::select
   */
  public function testSelectThrowsSearchApiExceptionWithErrorMsgOnNon200(): void {
    $body = (string) file_get_contents(__DIR__ . '/../../../../../solr-ref/responses/err_bad_sort.json');
    $client = $this->clientWithResponses([new Response(400, [], $body)]);

    $this->expectException(SearchApiException::class);
    $this->expectExceptionMessage('can not sort on a field w/o docValues unless it is indexed=true uninvertible=true and the type supports Uninversion: body');

    $client->select(['q' => '*:*', 'sort' => 'body desc']);
  }

  /**
   * @covers ::update
   */
  public function testUpdateThrowsSearchApiExceptionWithErrorMsgOnNon200(): void {
    $body = (string) file_get_contents(__DIR__ . '/../../../../../solr-ref/responses/err_bad_sort.json');
    $client = $this->clientWithResponses([new Response(400, [], $body)]);

    $this->expectException(SearchApiException::class);
    $this->expectExceptionMessage('can not sort on a field w/o docValues unless it is indexed=true uninvertible=true and the type supports Uninversion: body');

    $client->update(['add' => ['doc' => ['id' => 'x']]]);
  }

  /**
   * @covers ::ping
   */
  public function testPingReturnsTrueOn200(): void {
    $client = $this->clientWithResponses([new Response(200, [], '{"status":"OK"}')]);
    $this->assertTrue($client->ping());
  }

  /**
   * Both select and ping are protected routes when Wayfinder HTTP Basic
   * authentication is configured. The header value is asserted exactly so a
   * future change cannot silently choose a different authentication scheme.
   *
   * @covers ::select
   * @covers ::ping
   */
  public function testSelectAndPingSendBasicCredentialsWhenBothAreConfigured(): void {
    $history = [];
    $client = $this->clientWithHistory([
      new Response(200, [], '{"response":{"numFound":0,"docs":[]}}'),
      new Response(200, [], '{"status":"OK"}'),
    ], $history, 'alice', 's3cr3t');

    $client->select(['q' => '*:*']);
    $this->assertTrue($client->ping());

    $this->assertCount(2, $history);
    $this->assertSame('Basic ' . base64_encode('alice:s3cr3t'), $history[0]['request']->getHeaderLine('Authorization'));
    $this->assertSame('Basic ' . base64_encode('alice:s3cr3t'), $history[1]['request']->getHeaderLine('Authorization'));
  }

  /**
   * @dataProvider incompleteCredentialsProvider
   * @covers ::select
   * @covers ::ping
   */
  public function testSelectAndPingOmitAuthorizationWithoutACompleteCredentialPair(?string $username, ?string $password): void {
    $history = [];
    $client = $this->clientWithHistory([
      new Response(200, [], '{"response":{"numFound":0,"docs":[]}}'),
      new Response(200, [], '{"status":"OK"}'),
    ], $history, $username, $password);

    $client->select(['q' => '*:*']);
    $this->assertTrue($client->ping());

    $this->assertCount(2, $history);
    $this->assertFalse($history[0]['request']->hasHeader('Authorization'));
    $this->assertFalse($history[1]['request']->hasHeader('Authorization'));
  }

  /**
   * @return array<string, array{0: ?string, 1: ?string}>
   */
  public static function incompleteCredentialsProvider(): array {
    return [
      'absent' => [NULL, NULL],
      'username only' => ['alice', ''],
      'password only' => ['', 's3cr3t'],
    ];
  }

  /**
   * Wayfinder deliberately returns its normal Solr-compatible JSON error
   * envelope for an authentication failure, rather than Solr's HTML body.
   *
   * @covers ::select
   */
  public function testSelectTurnsWayfinderAuthenticationEnvelopeIntoSearchApiException(): void {
    $client = $this->clientWithResponses([
      new Response(401, [], '{"responseHeader":{"status":401,"QTime":0},"error":{"metadata":["error-class","wayfinder::Error","root-error-class","wayfinder::AuthenticationError"],"msg":"authentication required","code":401}}'),
    ]);

    $this->expectException(SearchApiException::class);
    $this->expectExceptionMessage('authentication required');

    $client->select(['q' => '*:*']);
  }

  /**
   * @covers ::ping
   */
  public function testPingReturnsFalseOnNon200(): void {
    $client = $this->clientWithResponses([new Response(500, [], '{"status":"error"}')]);
    $this->assertFalse($client->ping());
  }

  /**
   * @covers ::ping
   */
  public function testPingReturnsFalseOnConnectionErrorRatherThanThrowing(): void {
    $mock = new MockHandler([
      new \GuzzleHttp\Exception\ConnectException(
        'Connection refused',
        new \GuzzleHttp\Psr7\Request('GET', 'http://localhost:8983/solr/mycore/admin/ping')
      ),
    ]);
    $handlerStack = HandlerStack::create($mock);
    $httpClient = new Client(['handler' => $handlerStack]);
    $client = new WayfinderClient($httpClient, 'http://localhost:8983/solr/mycore');

    $this->assertFalse($client->ping());
  }

  /**
   * @covers ::select
   */
  public function testSelectThrowsSearchApiExceptionOnConnectException(): void {
    $mock = new MockHandler([
      new \GuzzleHttp\Exception\ConnectException(
        'Connection refused',
        new \GuzzleHttp\Psr7\Request('GET', 'http://localhost:8983/solr/mycore/select')
      ),
    ]);
    $handlerStack = HandlerStack::create($mock);
    $httpClient = new Client(['handler' => $handlerStack]);
    $client = new WayfinderClient($httpClient, 'http://localhost:8983/solr/mycore');

    $this->expectException(SearchApiException::class);
    $client->select(['q' => '*:*']);
  }

  /**
   * @covers ::update
   */
  public function testUpdateThrowsSearchApiExceptionOnConnectException(): void {
    $mock = new MockHandler([
      new \GuzzleHttp\Exception\ConnectException(
        'Connection refused',
        new \GuzzleHttp\Psr7\Request('POST', 'http://localhost:8983/solr/mycore/update')
      ),
    ]);
    $handlerStack = HandlerStack::create($mock);
    $httpClient = new Client(['handler' => $handlerStack]);
    $client = new WayfinderClient($httpClient, 'http://localhost:8983/solr/mycore');

    $this->expectException(SearchApiException::class);
    $client->update(['add' => ['doc' => ['id' => 'x']]]);
  }

  /**
   * @covers ::select
   */
  public function testSelectSerializesMultipleFilterQueriesAsRepeatedFqParameters(): void {
    $history = [];
    $mock = new MockHandler([new Response(200, [], '{"response":{"numFound":0,"docs":[]}}')]);
    $handlerStack = HandlerStack::create($mock);
    $handlerStack->push(\GuzzleHttp\Middleware::history($history));
    $client = new WayfinderClient(new Client(['handler' => $handlerStack]), 'http://localhost:8983/solr/mycore');

    $client->select(['q' => '*:*', 'fq' => ['index_id:"my_index"', 'ss_status:"published"']]);

    $query = $history[0]['request']->getUri()->getQuery();
    $this->assertSame('q=%2A%3A%2A&fq=index_id%3A%22my_index%22&fq=ss_status%3A%22published%22&wt=json', $query);
    $this->assertStringNotContainsString('fq%5B', $query);
  }

  /**
   * @covers ::update
   */
  public function testUpdatePassesCommitWithinAsQueryParamNotBodyKey(): void {
    $history = [];
    $mock = new MockHandler([new Response(200, [], '{"responseHeader":{"status":0}}')]);
    $handlerStack = HandlerStack::create($mock);
    $handlerStack->push(\GuzzleHttp\Middleware::history($history));
    $httpClient = new Client(['handler' => $handlerStack]);
    $client = new WayfinderClient($httpClient, 'http://localhost:8983/solr/mycore');

    $client->update(['add' => ['doc' => ['id' => 'x']]], ['commitWithin' => 1000]);

    $capturedRequest = $history[0]['request'];
    parse_str($capturedRequest->getUri()->getQuery(), $query);
    $this->assertSame('1000', $query['commitWithin']);

    $body = json_decode((string) $capturedRequest->getBody(), TRUE);
    $this->assertArrayNotHasKey('commitWithin', $body['add']);
  }

  /**
   * M4 (issue #78): WayfinderClient needs a select()-shaped sibling for
   * "GET {core}/mlt" (plan doc architecture table line 28: "GET
   * /solr/{core}/mlt | q, df, fl, rows, start, mlt.*"). Success envelope
   * shape ("match"/"response" keys, both numFound/docs) is ground truth from
   * solr-ref/responses/mlt_baseline.json.
   *
   * @covers ::mlt
   */
  public function testMltReturnsDecodedBodyOn200(): void {
    $body = (string) file_get_contents(__DIR__ . '/../../../../../solr-ref/responses/mlt_baseline.json');
    $client = $this->clientWithResponses([new Response(200, [], $body)]);

    $result = $client->mlt(['q' => 'id:"mlt1"', 'mlt.fl' => 'body,category']);

    $this->assertSame('mlt1', $result['match']['docs'][0]['id']);
  }

  /**
   * @covers ::mlt
   */
  public function testMltThrowsSearchApiExceptionWithErrorMsgOnNon200(): void {
    $body = (string) file_get_contents(__DIR__ . '/../../../../../solr-ref/responses/err_bad_sort.json');
    $client = $this->clientWithResponses([new Response(400, [], $body)]);

    $this->expectException(SearchApiException::class);
    $this->expectExceptionMessage('can not sort on a field w/o docValues unless it is indexed=true uninvertible=true and the type supports Uninversion: body');

    $client->mlt(['q' => 'id:"mlt1"']);
  }

  /**
   * @covers ::mlt
   */
  public function testMltRequestsTheMltEndpointNotSelect(): void {
    $history = [];
    $mock = new MockHandler([new Response(200, [], '{"match":{"numFound":0,"docs":[]},"response":{"numFound":0,"docs":[]}}')]);
    $handlerStack = HandlerStack::create($mock);
    $handlerStack->push(\GuzzleHttp\Middleware::history($history));
    $client = new WayfinderClient(new Client(['handler' => $handlerStack]), 'http://localhost:8983/solr/mycore');

    $client->mlt(['q' => 'id:"mlt1"', 'mlt.fl' => 'body']);

    $this->assertSame('/solr/mycore/mlt', $history[0]['request']->getUri()->getPath());
  }

}
