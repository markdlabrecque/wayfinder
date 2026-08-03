<?php

declare(strict_types=1);

namespace Drupal\Tests\search_api_wayfinder\Unit;

use Drupal\Core\Form\FormStateInterface;
use Drupal\search_api_wayfinder\ExtractFileValidator;
use Drupal\search_api_wayfinder\Plugin\search_api\processor\FileExtraction;
use Drupal\search_api_wayfinder\Plugin\search_api\processor\LinkedFileExtraction;
use PHPUnit\Framework\TestCase;

/**
 * Tests the extraction settings form and config defaults (issue #266): the
 * indexability rules and extraction settings from #264 are exposed as
 * processor configuration, with sensible defaults so the processor is useful
 * with no configuration at all.
 *
 * The form lives on {@see FileExtractionProcessorBase} so both the attached-file
 * (FileExtraction) and linked-file (LinkedFileExtraction) processors share one
 * set of settings and one form. Every test runs against BOTH processors to pin
 * that: a regression that only wired the form onto one subclass would slip a
 * single-processor assertion, but not both.
 *
 * Scope note: #266 ships the form + schema + defaults (the acceptance). The
 * processor wiring that *consumes* this config in addFieldValues() is the next
 * slice -- a form that stored settings nothing reads would be dead UI, and #264's
 * follow-up explicitly left the isFileIndexable/limitToAllowedNumber/limitBytes
 * call sites to a separate change. This test pins the form's contract so that
 * wiring change has something stable to read.
 *
 * @group search_api_wayfinder
 */
class ExtractionSettingsFormTest extends TestCase {

  /**
   * Both processors, each constructed with empty config so the constructor
   * merges in defaultConfiguration(), and a no-op string translation so $this->t()
 * in the form does not hit a half-bootstrapped \Drupal.
   *
   * @return array<string, \Drupal\search_api_wayfinder\Plugin\search_api\processor\FileExtractionProcessorBase>
   *   Both processors keyed by a short label, ready to assert against.
   */
  private function processors(): array {
    $file = new FileExtraction(
      [],
      'wayfinder_file_extraction',
      ['id' => 'wayfinder_file_extraction', 'label' => 'Wayfinder file extraction'],
    );
    $linked = new LinkedFileExtraction(
      [],
      'wayfinder_linked_file_extraction',
      ['id' => 'wayfinder_linked_file_extraction', 'label' => 'Wayfinder linked file extraction'],
    );
    foreach ([$file, $linked] as $processor) {
      $processor->setStringTranslation($this->createMock(\Drupal\Core\StringTranslation\TranslationInterface::class));
    }
    return ['file' => $file, 'linked' => $linked];
  }

  /**
   * The form exposes one control per setting, each defaulting to its
   * defaultConfiguration() value, so an admin sees sensible values with no
   * prior configuration. This is the "useful with no configuration at all"
   * acceptance criterion.
   */
  public function testBuildConfigurationFormExposesEverySettingWithSensibleDefaults(): void {
    $defaults = (new FileExtraction(
      [],
      'wayfinder_file_extraction',
      ['id' => 'wayfinder_file_extraction', 'label' => 'x'],
    ))->defaultConfiguration();

    foreach ($this->processors() as $label => $processor) {
      $form = $processor->buildConfigurationForm([], $this->createMock(FormStateInterface::class));

      // One control per setting, keyed exactly by the config key the processor
      // (and the #264 validator) reads -- a typo here silently disconnects the
      // form from the behaviour.
      $this->assertSame('radios', $form['extraction_mode']['#type'], $label);
      $this->assertSame($defaults['extraction_mode'], $form['extraction_mode']['#default_value'], $label);
      $this->assertSame(['inline', 'queue'], array_keys($form['extraction_mode']['#options']), $label);

      $this->assertSame('textarea', $form['excluded_extensions']['#type'], $label);

      $this->assertSame('textfield', $form['max_filesize']['#type'], $label);
      $this->assertSame($defaults['max_filesize'], $form['max_filesize']['#default_value'], $label);

      $this->assertSame('checkbox', $form['excluded_private']['#type'], $label);
      $this->assertSame($defaults['excluded_private'], $form['excluded_private']['#default_value'], $label);

      $this->assertSame('number', $form['number_indexed']['#type'], $label);
      $this->assertSame($defaults['number_indexed'], $form['number_indexed']['#default_value'], $label);
      $this->assertSame(0, $form['number_indexed']['#min'], $label);

      $this->assertSame('textfield', $form['number_first_bytes']['#type'], $label);
      $this->assertSame($defaults['number_first_bytes'], $form['number_first_bytes']['#default_value'], $label);
    }
  }

  /**
   * The default excluded-extensions list is the #264 constant, verbatim: the
   * form must not silently substitute a different default. And it must DISPLAY
   * that space-separated list one extension per line (the textarea UX), so an
   * admin editing it does not have to parse a space-delimited blob.
   */
  public function testExcludedExtensionsDefaultsToTheValidatorConstantOnePerLine(): void {
    foreach ($this->processors() as $label => $processor) {
      $form = $processor->buildConfigurationForm([], $this->createMock(FormStateInterface::class));

      $expected = explode(' ', ExtractFileValidator::DEFAULT_EXCLUDED_EXTENSIONS);
      $this->assertSame(
        $expected,
        explode("\n", $form['excluded_extensions']['#default_value']),
        $label,
      );
    }
  }

  /**
   * submitConfigurationForm() stores every setting under its config key, and
   * normalises excluded_extensions to a space-separated, lowercase,
   * de-duplicated, dot-stripped list -- the exact shape #264's
   * ExtractFileValidator::getExcludedMimes() will explode(' ', ...) on.
   *
   * The "PNG .jpg jpg" input is the load-bearing normalisation case: a user
   * typing mixed case, leading dots, and a duplicate must collapse to
   * 'png jpg', not be stored verbatim.
   */
  public function testSubmitConfigurationFormStoresAndNormalisesEverySetting(): void {
    foreach ($this->processors() as $label => $processor) {
      $form_state = $this->formStateWithValues([
        'extraction_mode' => 'queue',
        'excluded_extensions' => "PNG\n.jpg\njpg\n\n",
        'max_filesize' => '5 MB',
        'excluded_private' => FALSE,
        'number_indexed' => '3',
        'number_first_bytes' => '1 MB',
      ]);

      $form = [];
      $processor->submitConfigurationForm($form, $form_state);
      $config = $processor->getConfiguration();

      $this->assertSame('queue', $config['extraction_mode'], $label);
      $this->assertSame('png jpg', $config['excluded_extensions'], $label);
      $this->assertSame('5 MB', $config['max_filesize'], $label);
      $this->assertSame(FALSE, $config['excluded_private'], $label);
      $this->assertSame(3, $config['number_indexed'], $label);
      $this->assertSame('1 MB', $config['number_first_bytes'], $label);
    }
  }

  /**
   * An empty excluded-extensions box must round-trip to an empty string (no
   * restriction), not 'Array' or a stray newline -- the validator treats an
   * empty list as "use the default", so the form must let an admin clear it.
   */
  public function testSubmitConfigurationFormAllowsClearingExcludedExtensions(): void {
    foreach ($this->processors() as $label => $processor) {
      $form_state = $this->formStateWithValues(['excluded_extensions' => '']);
      $form = [];
      $processor->submitConfigurationForm($form, $form_state);

      $this->assertSame('', $processor->getConfiguration()['excluded_extensions'], $label);
    }
  }

  /**
   * validateConfigurationForm() rejects byte-size strings that do not parse
   * (Bytes::toNumber() returns 0 for garbage) while accepting '0' / '' as
   * "no restriction". A typo like '5mb' (no space) or 'huge' must surface as a
   * form error on the right element, not be silently stored as a 0-byte cap.
   *
   * The valid '5 MB' on number_first_bytes proves the validator distinguishes
   * parseable from unparseable -- it must not flag the good field.
   */
  public function testValidateConfigurationFormRejectsUnparseableByteSizes(): void {
    foreach ($this->processors() as $label => $processor) {
      $form_state = $this->formStateWithValues([
        'max_filesize' => 'huge',
        'number_first_bytes' => '5 MB',
      ]);
      // The load-bearing assertion: the bad max_filesize is flagged, the good
      // number_first_bytes is not, and exactly one error is raised.
      $form_state->expects($this->once())
        ->method('setErrorByName')
        ->with('max_filesize', $this->isInstanceOf(\Drupal\Core\StringTranslation\TranslatableMarkup::class));

      $form = [];
      $processor->validateConfigurationForm($form, $form_state);
    }
  }

  /**
   * '0' and '' are valid (they mean "no restriction"), so the validator must
   * accept them and raise no error. This guards against a too-eager validator
   * that rejects every non-positive parse, which would forbid the documented
   * "no limit" values.
   */
  public function testValidateConfigurationFormAcceptsZeroAndEmptyAsNoRestriction(): void {
    foreach ($this->processors() as $label => $processor) {
      $form_state = $this->formStateWithValues([
        'max_filesize' => '0',
        'number_first_bytes' => '',
      ]);
      $form_state->expects($this->never())->method('setErrorByName');

      $form = [];
      $processor->validateConfigurationForm($form, $form_state);
    }
  }

  /**
   * Builds a FormStateInterface double backed by a flat values map, so
   * getValue()/getValues() round-trip the submitted form without a Drupal form
   * builder. setErrorByName remains a mock an individual test can expect on.
   */
  private function formStateWithValues(array $values): FormStateInterface {
    $form_state = $this->createMock(FormStateInterface::class);
    $form_state->method('getValue')->willReturnCallback(
      fn ($key, $default = NULL) => array_key_exists($key, $values) ? $values[$key] : $default,
    );
    $form_state->method('getValues')->willReturn($values);
    return $form_state;
  }

}
