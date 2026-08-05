> **Historical implementation record.** This completed spec does not define current requirements or future work.

# #359 — server consumes only the first `spellcheck.dictionary` value

Branch: `359-spellcheck-multi-dictionary`. Group A.

## The defect

`spellcheck.dictionary` is repeatable — a Solr client may send it several times
to consult several dictionaries in one request. Wayfinder reads only the first
value; see the comment at `src/lib.rs:288` ("the suggestion path uses its first
value, matching Solr's capture").

Since #342 landed language-aware spellcheck sinks (`spellcheck_en`,
`spellcheck_de`, ...), a multilingual query that legitimately asks for several
dictionaries silently gets suggestions from one. Silently is the problem: no
error, no warning, just a narrower result than the client asked for.

## Verify before implementing — this one has a real trap

The existing comment says the first-value behaviour matches **Solr's capture**.
Before changing anything, find the fixture that comment refers to and read it.
Two possibilities, with opposite conclusions:

- The capture only ever sent **one** `spellcheck.dictionary`, and the comment
  means "consistent with a capture that never exercised this". Then the
  first-value behaviour is an untested assumption and this issue is a real bug.
- The capture sent **several** and Solr genuinely used only the first. Then
  Wayfinder is correct, the issue's premise is wrong, and the work is to
  document that rather than to change behaviour.

**Do not skip this.** Grep `solr-ref/responses/` and `solr-ref/manifest.tsv` for
spellcheck fixtures and establish which case holds. Report the answer in the PR
regardless of which way it goes.

If no existing fixture settles it, the question needs a capture: issue a
`select` with two `spellcheck.dictionary` values against real `solr:9` with two
configured dictionaries and see what comes back. Append the block at the **end**
of `solr-ref/capture.sh`, use `capture.sh --only <prefix>`, and commit the new
fixtures before doing anything else — untracked fixtures are not restored by
`git checkout -- solr-ref/`.

## Scope, if the premise holds

Consume every `spellcheck.dictionary` value, consulting each named dictionary
and merging the results into one response.

The merge semantics are the real design question, and the fixture decides them,
not your judgement:

- how per-term suggestions from several dictionaries combine (concatenated in
  dictionary order? interleaved? deduplicated?)
- how `spellcheck.count` applies — per dictionary, or across the merged set
- what happens when one named dictionary does not exist: does the whole request
  error, or is the missing one skipped?

If the capture does not answer one of these, capture the case that does. Do not
invent a rule and do not average across plausible ones.

## Scope, if the premise is wrong

Replace the comment at `src/lib.rs:288` with one that cites the fixture proving
first-value-only is Solr's real behaviour, and close the issue with that
evidence. Add a guard test asserting the multi-value case still behaves the way
the fixture shows, so the question is not reopened by memory later.

## Testing

Tests first, red, derived from fixtures. Whichever branch you are on, the suite
must end up containing a test that exercises **two or more**
`spellcheck.dictionary` values — today there is none, which is how this survived.

This is compatibility-guard code, so mutation-test it: break the merge
deliberately (drop the second dictionary's results), confirm a test catches it,
revert.

## Files

**You own:** `src/lib.rs` spellcheck handling, the spellcheck test suite,
`solr-ref/capture.sh` (append at end only), `solr-ref/manifest.tsv` (core-relative
GETs only — anything else goes in `manifest-errors.tsv`).

**Siblings own:** the Drupal module files (#358, #360, #361, #362).
`SELECT_PARAMS` belongs to Group B — if you need to add a param there, flag it
rather than editing, since #350 and #353 are both in that list.

**Dependency:** #351 (`/autocomplete`) needs real spellcheck and sequences after
this. Land it cleanly.

## Definition of done

- The premise is settled with fixture evidence, reported in the PR either way.
- A multi-dictionary test exists and is derived from a fixture.
- Mutation test performed and reported.
- `cargo test`, `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`
  clean.
