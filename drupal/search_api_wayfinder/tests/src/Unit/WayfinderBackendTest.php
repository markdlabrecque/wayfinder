<?php

declare(strict_types=1);

namespace Drupal\Tests\search_api_wayfinder\Unit;

use Drupal\search_api_wayfinder\Plugin\search_api\backend\WayfinderBackend;
use PHPUnit\Framework\TestCase;

/**
 * Tests WayfinderBackend's plugin-level feature flag only.
 *
 * `getSupportedFeatures()` needs no container/server wiring (it is a plain
 * getter), so the plugin can be instantiated directly with
 * `BackendPluginBase`'s bare constructor -- the same pattern
 * `WayfinderBackend::create()` uses on top of, just without the container
 * dependency injection `create()` adds (http_client), which this method
 * never touches.
 *
 * M3 (plan doc "Backend plugin contract": `getSupportedFeatures():
 * ['search_api_facets', 'search_api_mlt']`) adds `search_api_facets` now;
 * `search_api_mlt` is M4's job and is deliberately not asserted here.
 *
 * @coversDefaultClass \Drupal\search_api_wayfinder\Plugin\search_api\backend\WayfinderBackend
 * @group search_api_wayfinder
 */
class WayfinderBackendTest extends TestCase {

  /**
   * @covers ::getSupportedFeatures
   */
  public function testGetSupportedFeaturesIncludesSearchApiFacets(): void {
    $backend = new WayfinderBackend([], 'wayfinder', []);

    $this->assertContains('search_api_facets', $backend->getSupportedFeatures());
  }

}
