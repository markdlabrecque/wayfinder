//! Deterministic corpus generator for the benchmark suite (issue #13).
//!
//! Field shape mirrors what the rest of the repo already tests against
//! rather than inventing a new schema: `id` (string, unique key), `title`
//! and `body` (text_en, per `tests/edismax.rs`'s `EDISMAX_SCHEMA_TOML`), and
//! `category` (multi_valued string, per `tests/common/mod.rs`'s
//! tracer-bullet schema) -- together enough to drive a facet+filter+
//! highlight query set, which is what PRD §8's p95 metric measures.
//!
//! Determinism comes from a hand-rolled splitmix64 PRNG (`std` only, per
//! `Cargo.toml`'s no-dependency constraint) seeded once from `(seed, size)`
//! and stepped forward for every word/category choice, so the same
//! `(seed, size)` always walks the exact same sequence of draws.

/// A single generated document.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Doc {
    pub id: String,
    pub title: String,
    pub body: String,
    pub category: Vec<String>,
}

/// splitmix64: minimal, fast, deterministic -- see Vigna's public-domain
/// reference algorithm. Not cryptographic; purely a reproducible sequence
/// generator for synthetic corpus content.
struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    fn new(seed: u64) -> Self {
        SplitMix64 { state: seed }
    }

    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E3779B97F4A7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
        z ^ (z >> 31)
    }

    /// A value in `0..bound` (bound must be > 0).
    fn next_range(&mut self, bound: usize) -> usize {
        (self.next_u64() % bound as u64) as usize
    }
}

const TITLE_WORDS: &[&str] = &[
    "rocket",
    "launch",
    "mission",
    "control",
    "orbit",
    "satellite",
    "gravity",
    "engine",
    "capsule",
    "station",
    "voyage",
    "signal",
    "descent",
    "ascent",
    "thruster",
    "payload",
];

const BODY_WORDS: &[&str] = &[
    "the",
    "quick",
    "brown",
    "fox",
    "jumps",
    "over",
    "lazy",
    "dog",
    "system",
    "returns",
    "a",
    "result",
    "after",
    "processing",
    "every",
    "record",
    "in",
    "sequence",
    "and",
    "verifying",
    "each",
    "field",
    "against",
    "expected",
    "output",
    "before",
    "moving",
    "on",
    "to",
    "next",
    "batch",
    "of",
    "work",
    "items",
    "queued",
    "for",
    "execution",
    "today",
    "yesterday",
    "tomorrow",
];

const CATEGORIES: &[&str] = &[
    "animals", "classic", "garden", "misc", "science", "history", "sports", "music",
];

fn gen_words(rng: &mut SplitMix64, words: &[&str], count: usize) -> String {
    (0..count)
        .map(|_| words[rng.next_range(words.len())])
        .collect::<Vec<_>>()
        .join(" ")
}

fn gen_categories(rng: &mut SplitMix64) -> Vec<String> {
    let n = rng.next_range(4); // 0..=3 categories
    let mut cats = Vec::with_capacity(n);
    for _ in 0..n {
        cats.push(CATEGORIES[rng.next_range(CATEGORIES.len())].to_string());
    }
    cats
}

/// Generates `size` documents deterministically from `seed`. Same
/// `(seed, size)` always produces byte-identical output; different seed or
/// size diverges.
pub fn generate(seed: u64, size: usize) -> Vec<Doc> {
    // Fold size into the initial state so identical seeds at different
    // sizes don't share a prefix of draws by construction.
    let mut rng = SplitMix64::new(seed ^ (size as u64).wrapping_mul(0x2545F4914F6CDD1D));

    let mut docs = Vec::with_capacity(size);
    for i in 0..size {
        let title_len = 3 + rng.next_range(4); // 3..=6 words
        let body_len = 20 + rng.next_range(21); // 20..=40 words
        let title = gen_words(&mut rng, TITLE_WORDS, title_len);
        let body = gen_words(&mut rng, BODY_WORDS, body_len);
        let category = gen_categories(&mut rng);

        docs.push(Doc {
            id: format!("doc{i}"),
            title,
            body,
            category,
        });
    }
    docs
}

/// A stable hash of the generated doc set: same docs (in the same order)
/// always hash to the same value, and the count is folded in explicitly so
/// two runs that happen to agree on content but differ on size can't
/// collide.
pub fn content_hash(docs: &[Doc]) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut hasher = DefaultHasher::new();
    docs.len().hash(&mut hasher);
    for doc in docs {
        doc.hash(&mut hasher);
    }
    hasher.finish()
}
