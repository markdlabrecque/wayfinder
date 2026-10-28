//! Both engines must see the identical request sequence, run after run, or
//! the wf-bench comparison is meaningless (issue #440).

use wayfinder_bench::terms::pools;
use wayfinder_bench::wiki::{WikiDoc, initial, len_bucket};

fn doc(id: usize, body: &str) -> WikiDoc {
    WikiDoc {
        id: id.to_string(),
        title: "T".into(),
        body: body.into(),
        len: body.len() as i64,
        len_bucket: len_bucket(body.len() as i64),
        initial: initial("T"),
    }
}

#[test]
fn filled_queries_are_stable_and_fully_substituted() {
    // 12 words, so each term lands in ~17% of docs: under the 20% pool cap.
    let words = [
        "river", "bridge", "castle", "harbor", "valley", "temple", "forest", "island", "canyon",
        "meadow", "glacier", "desert",
    ];
    let docs: Vec<WikiDoc> = (0..60)
        .map(|i| {
            doc(
                i,
                &format!(
                    "the {} {} of the {}",
                    words[i % 12],
                    words[(i + 1) % 12],
                    words[(i + 2) % 12]
                ),
            )
        })
        .collect();
    let template = "q={t} AND {t2}&p=\"{bigram}\"&start={start}&r=[{lo} TO {hi}]";
    let a = pools(&docs, 42);
    let b = pools(&docs, 42);
    let filled: Vec<String> = (0..20).map(|i| a.fill(template, i)).collect();
    assert_eq!(
        filled,
        (0..20).map(|i| b.fill(template, i)).collect::<Vec<_>>()
    );
    assert!(
        filled
            .iter()
            .all(|f| !f.contains('{') && !f.contains("the"))
    );
    assert!(filled.iter().any(|f| f != &filled[0]), "requests must vary");
}
