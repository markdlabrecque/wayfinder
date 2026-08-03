# #278 — close the `&mut Budget` encapsulation gap

Closes the encapsulation gap #257 round 2 left open and #258 deferred for a
second time. The trigger the deferral named — "the first extractor that does
not need to read its own output back" giving a second real call site to
design `output_text()`'s new shape against — has arrived.

## The gap

#257 follow-up item E made the six structural counters unforgeable: private,
`Cell`-backed, reachable only through `&self` delegating methods that cannot
raise a limit or decrement a cumulative counter. What stayed open was one
level up: `Extractor::extract` took `&mut Budget`, so an in-tree extractor
could reassign the whole budget wholesale —

```rust
*budget = Budget::new(ExtractLimits { .. });
```

— and mint itself fresh counters and a fresh deadline. Every individual
counter was safe; the binding holding them was not. The `Budget` type doc
said so explicitly and named this as the residual review burden.

## Fix

Move the budget's mutable output state behind a `RefCell` so the mutating
methods can take `&self`, then drop the trait to a shared reference.

- `Budget::output` is now a private `RefCell<BudgetOutput>` where
  `BudgetOutput { text: String, scalars: usize }` is the interior-mutable
  core (the two are always charged together, so one borrow covers both).
- `push_str`, `charge_output`, `output_text`, and `output_scalars` now take
  `&self`.
- `Extractor::extract` (and the standalone `extract` / `extract_document`,
  plus every helper threaded from an extractor: `walk_docx_body`,
  `collect_zip_parts`, `refine_zip_kind`, `finish_office`, `walk_pptx_slide`,
  `RtfWalker`, `HtmlSink`/`SinkState`, `push_text`, `push_bounded`, …) now
  take `&Budget`. None of them ever needed the `&mut`; the `&mut` was only
  there because `push_str` used to require it.

`Budget` stays `Send` and `!Sync` — a `RefCell<Send + Sync>` is `Send` and
`!Sync`, exactly like the `Cell`s already there — so the `assert_send`
const-check and the `## Threading` promise are unchanged.

## The `output_text()` shape decision (written down)

The deferral named three options for what `output_text()` could hand back
once the buffer was behind a `RefCell`: a `Ref<'_, String>`, a
closure-taking `with_output_text`, or an unconditional copy. The decision:

**`output_text(&self) -> Ref<'_, str>`** (a `Ref::map` of the borrow).

Rationale: every in-tree read-back already copies its slice out
(`output_text()[start..].to_string()`), so a `Ref` preserves that one-copy
profile while a forced copy would also allocate for the length-probe callers
and a closure would reshape all eleven call sites. A `Ref<'_, str>` derefs to
`str`, so `.len()`, `[start..]`, and `.to_string()` all keep working
unchanged — the only edits outside `Budget` itself were two call-site
cleanups clippy asked for and one internal test assertion
(`assert_eq!(&*budget.output_text(), "ab", …)`).

The one rule callers must keep — **drop the `Ref` before pushing more
output** — is already true of every in-tree call site (a length probe, or a
`[start..]` slice copied out in the same expression). It is documented on the
method, and the trait's `compile_fail` doctest is the record that an
extractor never gets the `&mut` it would need to break it.

## Tests / evidence

Two guards, complementary:

1. **`compile_fail` doctest on `Extractor`** — the illustrative guard the
   issue asked for. It implements a `Rogue` extractor whose `extract` tries
   `*budget = Budget::new(…)`. Under `&Budget` this fails to compile, so the
   doctest passes; the failure is the property.
2. **`extractor_extract_accepts_only_a_shared_budget_reference`** — a
   compile-time guard expressed as a `#[test]`. It calls
   `PlainTextExtractor.extract(&input, &budget)` with a **shared** reference.
   The test binary builds only while the trait takes `&Budget`; a revert to
   `&mut Budget` stops `&budget` coercing and the test file fails to build.

### Mutation test (per the working agreement)

The guard's whole value is failing correctly, so it was mutation-tested two
ways, both reverted:

- **Full revert** (`&Budget` → `&mut Budget` on the trait, every impl, and the
  doctest's impl signature — a realistic regression): the build breaks at
  every shared-`&budget` call site, including the dedicated unit guard above
  and the `/update/extract` route (`src/lib.rs`).
  `error[E0308]: mismatched types … expected mutable reference &mut Budget,
  found reference &Budget`.
- **Surgical doctest mutation**: with the green code, weakening the doctest's
  offending line to a statement that compiles under `&Budget`
  (`let _ = budget.limits();`) makes the doctest fail:
  `Test compiled successfully, but it's marked compile_fail`. This proves the
  doctest's `compile_fail` is correctly tied to the reassignment line — the
  only thing keeping the snippet from compiling is the attempt to assign
  through a shared reference.

Caveat recorded honestly: the `compile_fail` doctest's impl signature is
coupled to the trait's, so a *partial* revert (trait only, doctest left on
`&Budget`) would surface as a signature-mismatch compile error rather than
the property error. The unit guard (#2) exists precisely to make a partial
revert loud too — it can't compile against a `&mut Budget` trait.

## Gates

`cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, and
`cargo test` (incl. doctests and the differential harness) are all green.

## Files

- `src/extract.rs` — `Budget` interior mutability, the `&Budget` trait +
  helpers, the rewritten "What is unforgeable" doc, the `output_text()`
  decision, and both guards.
- `src/lib.rs` — the `/update/extract` route no longer takes a `&mut` it did
  not need.
