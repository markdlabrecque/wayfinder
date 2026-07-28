//! A minimal "collect every match, in a fixed deterministic order" collector.
//!
//! The tracer bullet doesn't implement `sort` (explicitly out of scope, PRD
//! §7), so result order only needs to be *deterministic* and match Solr's
//! observed behaviour for a single-segment, equally-scored corpus: ties break
//! by ascending document order. Using our own collector instead of trusting
//! `TopDocs`' internal tie-break keeps that guarantee explicit rather than
//! incidental.

use tantivy::collector::{Collector, SegmentCollector};
use tantivy::{DocAddress, DocId, Score, SegmentOrdinal};

pub struct AllScoredHits;

impl Collector for AllScoredHits {
    type Fruit = Vec<(Score, DocAddress)>;
    type Child = AllScoredHitsSegmentCollector;

    fn for_segment(
        &self,
        segment_ord: SegmentOrdinal,
        _segment: &tantivy::SegmentReader,
    ) -> tantivy::Result<Self::Child> {
        Ok(AllScoredHitsSegmentCollector {
            segment_ord,
            hits: Vec::new(),
        })
    }

    fn requires_scoring(&self) -> bool {
        true
    }

    fn merge_fruits(
        &self,
        segment_fruits: Vec<Vec<(Score, DocAddress)>>,
    ) -> tantivy::Result<Self::Fruit> {
        let mut all: Vec<(Score, DocAddress)> = segment_fruits.into_iter().flatten().collect();
        all.sort_by(|a, b| {
            b.0.partial_cmp(&a.0)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.1.cmp(&b.1))
        });
        Ok(all)
    }
}

pub struct AllScoredHitsSegmentCollector {
    segment_ord: SegmentOrdinal,
    hits: Vec<(Score, DocAddress)>,
}

impl SegmentCollector for AllScoredHitsSegmentCollector {
    type Fruit = Vec<(Score, DocAddress)>;

    fn collect(&mut self, doc: DocId, score: Score) {
        self.hits
            .push((score, DocAddress::new(self.segment_ord, doc)));
    }

    fn harvest(self) -> Self::Fruit {
        self.hits
    }
}
