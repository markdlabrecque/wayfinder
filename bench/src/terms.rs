//! Query variety for `wf-bench` (issue #440). Warm runs must not repeat one
//! URL, or Solr answers from its queryResultCache and the benchmark measures
//! a cache lookup. Instead each request fills a `queries.tsv` template from
//! pools of real terms and word pairs drawn from the corpus, so every timed
//! request is a query the engine has not seen before.
//!
//! Placeholders: `{t}` and `{t2}` (two different terms), `{bigram}` (two
//! words that sit next to each other in the corpus, for phrase queries),
//! `{start}` (a paging offset), `{lo}` / `{hi}` (a 1000-wide `len` range).

use std::collections::HashMap;

use crate::wiki::WikiDoc;

/// Docs scanned to build the pools. Fixed, so the pools never depend on the
/// corpus size a run was started with.
const SAMPLE_DOCS: usize = 20_000;
const POOL: usize = 2_000;
const MIN_DF: u32 = 3;

// Kept out of the pools: both engines drop English stopwords, so they would
// only ever produce empty queries.
const STOPWORDS: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "but", "by", "for", "from", "had", "has", "have",
    "he", "her", "his", "if", "in", "into", "is", "it", "its", "no", "not", "of", "on", "or",
    "she", "such", "that", "the", "their", "then", "there", "these", "they", "this", "to", "was",
    "were", "which", "will", "with",
];

pub struct Pools {
    pub terms: Vec<String>,
    pub bigrams: Vec<String>,
    pub n_docs: usize,
}

/// splitmix64: a tiny, stable mixer, so the same (seed, index) always picks
/// the same terms.
fn mix(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

fn fnv(s: &str) -> u64 {
    s.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ b as u64).wrapping_mul(0x100_0000_01b3)
    })
}

fn words(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_ascii_lowercase)
}

fn useful(w: &str) -> bool {
    w.len() >= 3 && w.bytes().all(|b| b.is_ascii_lowercase()) && !STOPWORDS.contains(&w)
}

/// Picks `POOL` keys with doc frequency in `[MIN_DF, max_df]`, in an order
/// fixed by `seed` rather than by frequency, so common and rare terms mix.
fn pick(df: HashMap<String, u32>, max_df: u32, seed: u64) -> Vec<String> {
    let mut keys: Vec<_> = df
        .into_iter()
        .filter(|(_, n)| (MIN_DF..=max_df).contains(n))
        .map(|(k, _)| k)
        .collect();
    keys.sort_by_key(|k| (mix(seed ^ fnv(k)), k.clone()));
    keys.truncate(POOL);
    keys
}

pub fn pools(docs: &[WikiDoc], seed: u64) -> Pools {
    let sample = &docs[..docs.len().min(SAMPLE_DOCS)];
    let (mut terms, mut bigrams) = (HashMap::new(), HashMap::new());
    for d in sample {
        let ws: Vec<String> = words(&d.body).collect();
        let mut seen_t = std::collections::HashSet::new();
        let mut seen_b = std::collections::HashSet::new();
        for pair in ws.windows(2) {
            if useful(&pair[0]) && seen_t.insert(pair[0].clone()) {
                *terms.entry(pair[0].clone()).or_insert(0) += 1;
            }
            if useful(&pair[0]) && useful(&pair[1]) {
                let b = format!("{} {}", pair[0], pair[1]);
                if seen_b.insert(b.clone()) {
                    *bigrams.entry(b).or_insert(0) += 1;
                }
            }
        }
    }
    let max_df = (sample.len() as u32 / 5).max(MIN_DF);
    Pools {
        terms: pick(terms, max_df, seed),
        bigrams: pick(bigrams, max_df, seed),
        n_docs: docs.len(),
    }
}

impl Pools {
    /// Fills `template` for request number `i`. Same `(template, i)` always
    /// gives the same string.
    pub fn fill(&self, template: &str, i: u64) -> String {
        let r = |k: u64| mix(i.wrapping_mul(8).wrapping_add(k));
        let t = r(1) as usize % self.terms.len();
        let mut t2 = r(2) as usize % self.terms.len();
        if t2 == t {
            t2 = (t2 + 1) % self.terms.len();
        }
        let lo = r(4) % 20_000;
        template
            .replace("{t}", &self.terms[t])
            .replace("{t2}", &self.terms[t2])
            .replace(
                "{bigram}",
                &self.bigrams[r(3) as usize % self.bigrams.len()],
            )
            .replace(
                "{start}",
                &(r(5) % self.n_docs.saturating_sub(10).max(1) as u64).to_string(),
            )
            .replace("{lo}", &lo.to_string())
            .replace("{hi}", &(lo + 1000).to_string())
    }
}
