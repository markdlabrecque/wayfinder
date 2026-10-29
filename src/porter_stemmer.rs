//! Classic Porter stemming, as Solr's `_default` `text_en` runs it through
//! `PorterStemFilterFactory` (#477).
//!
//! This follows Lucene's `PorterStemmer`, a port of Martin Porter's reference
//! C, including that version's `-bli` -> `-ble` and `-logi` -> `-log`
//! departures from the 1980 paper. Tantivy's English stemmer is Snowball
//! (Porter2), which keeps the `s` of `Argus` and re-strips `-tion` from
//! `institutionally`, so `text_en` missed pages Solr finds.

/// Replaces `text` with its classic Porter stem. Words shorter than three
/// characters are left alone, as Lucene leaves them.
pub(crate) fn stem(text: &mut String) {
    if text.chars().nth(2).is_none() {
        return;
    }
    let mut word = Word {
        b: text.chars().collect(),
        j: 0,
    };
    word.step1ab();
    word.step1c();
    word.step2();
    word.step3();
    word.step4();
    word.step5();
    text.clear();
    text.extend(word.b);
}

struct Word {
    b: Vec<char>,
    /// Where the suffix the last successful [`Word::ends`] matched begins.
    j: usize,
}

impl Word {
    /// Whether `b[i]` is a consonant. `y` is one at the start of the word or
    /// after a vowel. Walking back over a run of `y` instead of recursing keeps
    /// a pathological token off the stack.
    fn cons(&self, mut i: usize) -> bool {
        let mut flipped = false;
        loop {
            match self.b[i] {
                'a' | 'e' | 'i' | 'o' | 'u' => return flipped,
                'y' if i > 0 => {
                    flipped = !flipped;
                    i -= 1;
                }
                _ => return !flipped,
            }
        }
    }

    /// [`Word::cons`] for each of `b[..end]`, in one pass.
    fn consonants(&self, end: usize) -> impl Iterator<Item = bool> + '_ {
        let mut previous = false;
        self.b[..end].iter().enumerate().map(move |(i, &c)| {
            previous = match c {
                'a' | 'e' | 'i' | 'o' | 'u' => false,
                'y' => i == 0 || !previous,
                _ => true,
            };
            previous
        })
    }

    /// Porter's measure: the number of vowel-consonant sequences in `b[..j]`.
    fn m(&self) -> usize {
        let mut n = 0;
        let mut after_vowel = false;
        for cons in self.consonants(self.j) {
            if cons && after_vowel {
                n += 1;
            }
            after_vowel = !cons;
        }
        n
    }

    fn vowel_in_stem(&self) -> bool {
        self.consonants(self.j).any(|cons| !cons)
    }

    /// Whether `b[i - 1]` and `b[i]` are the same consonant.
    fn double_consonant(&self, i: usize) -> bool {
        i >= 1 && self.b[i] == self.b[i - 1] && self.cons(i)
    }

    /// Whether `b[i - 2..=i]` is consonant-vowel-consonant and `b[i]` is not
    /// `w`, `x` or `y`.
    fn cvc(&self, i: usize) -> bool {
        i >= 2
            && self.cons(i)
            && !self.cons(i - 1)
            && self.cons(i - 2)
            && !matches!(self.b[i], 'w' | 'x' | 'y')
    }

    /// Whether the word ends with `suffix`; if so, sets `j` to where it begins.
    fn ends(&mut self, suffix: &str) -> bool {
        let Some(start) = self.b.len().checked_sub(suffix.len()) else {
            return false;
        };
        if !self.b[start..].iter().copied().eq(suffix.chars()) {
            return false;
        }
        self.j = start;
        true
    }

    /// Replaces everything from `j` on with `replacement`.
    fn set_to(&mut self, replacement: &str) {
        self.b.truncate(self.j);
        self.b.extend(replacement.chars());
    }

    /// Applies the first rule whose suffix ends the word, if what precedes
    /// that suffix has a positive measure. A matching rule stops the search
    /// even when its measure is zero (`rational` keeps `-tional`).
    fn replace_first(&mut self, rules: &[(&str, &str)]) {
        for (suffix, replacement) in rules {
            if self.ends(suffix) {
                if self.m() > 0 {
                    self.set_to(replacement);
                }
                return;
            }
        }
    }

    /// Plurals, `-ed` and `-ing`.
    fn step1ab(&mut self) {
        if self.b.last() == Some(&'s') {
            if self.ends("sses") {
                self.b.truncate(self.b.len() - 2);
            } else if self.ends("ies") {
                self.set_to("i");
            } else if self.b[self.b.len() - 2] != 's' {
                self.b.pop();
            }
        }
        if self.ends("eed") {
            if self.m() > 0 {
                self.b.pop();
            }
        } else if (self.ends("ed") || self.ends("ing")) && self.vowel_in_stem() {
            self.b.truncate(self.j);
            let last = self.b.len() - 1;
            if self.ends("at") {
                self.set_to("ate");
            } else if self.ends("bl") {
                self.set_to("ble");
            } else if self.ends("iz") {
                self.set_to("ize");
            } else if self.double_consonant(last) {
                if !matches!(self.b[last], 'l' | 's' | 'z') {
                    self.b.pop();
                }
            } else if self.m() == 1 && self.cvc(last) {
                self.set_to("e");
            }
        }
    }

    /// A terminal `y` becomes `i` when the stem has a vowel.
    fn step1c(&mut self) {
        if self.ends("y") && self.vowel_in_stem() {
            let last = self.b.len() - 1;
            self.b[last] = 'i';
        }
    }

    /// Double suffixes to single ones.
    fn step2(&mut self) {
        self.replace_first(&[
            ("ational", "ate"),
            ("tional", "tion"),
            ("enci", "ence"),
            ("anci", "ance"),
            ("izer", "ize"),
            ("bli", "ble"),
            ("alli", "al"),
            ("entli", "ent"),
            ("eli", "e"),
            ("ousli", "ous"),
            ("ization", "ize"),
            ("ation", "ate"),
            ("ator", "ate"),
            ("alism", "al"),
            ("iveness", "ive"),
            ("fulness", "ful"),
            ("ousness", "ous"),
            ("aliti", "al"),
            ("iviti", "ive"),
            ("biliti", "ble"),
            ("logi", "log"),
        ]);
    }

    /// `-ic-`, `-full`, `-ness` and the like.
    fn step3(&mut self) {
        self.replace_first(&[
            ("icate", "ic"),
            ("ative", ""),
            ("alize", "al"),
            ("iciti", "ic"),
            ("ical", "ic"),
            ("ful", ""),
            ("ness", ""),
        ]);
    }

    /// `-ant`, `-ence` and the like, when the stem's measure is above one.
    fn step4(&mut self) {
        const SUFFIXES: [&str; 19] = [
            "al", "ance", "ence", "er", "ic", "able", "ible", "ant", "ement", "ment", "ent", "ion",
            "ou", "ism", "ate", "iti", "ous", "ive", "ize",
        ];
        let Some(suffix) = SUFFIXES.into_iter().find(|suffix| self.ends(suffix)) else {
            return;
        };
        if suffix == "ion" && !(self.j > 0 && matches!(self.b[self.j - 1], 's' | 't')) {
            return;
        }
        if self.m() > 1 {
            self.b.truncate(self.j);
        }
    }

    /// A final `-e`, and `-ll` to `-l`, when the measure allows.
    fn step5(&mut self) {
        self.j = self.b.len();
        if self.b.last() == Some(&'e') {
            let m = self.m();
            if m > 1 || (m == 1 && !self.cvc(self.b.len() - 2)) {
                self.b.pop();
                // `e` is a vowel, so dropping it leaves the measure unchanged.
                self.j = self.b.len();
            }
        }
        let last = self.b.len() - 1;
        if self.b[last] == 'l' && self.double_consonant(last) && self.m() > 1 {
            self.b.pop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stemmed(word: &str) -> String {
        let mut text = word.to_owned();
        stem(&mut text);
        text
    }

    #[test]
    fn matches_classic_porter_outputs() {
        for (word, expected) in [
            ("caresses", "caress"),
            ("ponies", "poni"),
            ("ties", "ti"),
            ("cats", "cat"),
            ("feed", "feed"),
            ("agreed", "agre"),
            ("plastered", "plaster"),
            ("bled", "bled"),
            ("motoring", "motor"),
            ("sing", "sing"),
            ("conflated", "conflat"),
            ("troubled", "troubl"),
            ("sized", "size"),
            ("hopping", "hop"),
            ("falling", "fall"),
            ("hissing", "hiss"),
            ("fizzed", "fizz"),
            ("filing", "file"),
            ("happy", "happi"),
            ("sky", "sky"),
            ("relational", "relat"),
            ("conditional", "condit"),
            ("rational", "ration"),
            ("digitizer", "digit"),
            ("generalizations", "gener"),
            ("oscillators", "oscil"),
            ("days", "dai"),
            ("played", "plai"),
        ] {
            assert_eq!(stemmed(word), expected, "{word}");
        }
    }

    #[test]
    fn stems_the_words_snowball_split_from_solr() {
        assert_eq!(stemmed("argue"), stemmed("argus"));
        assert_eq!(stemmed("institutionally"), "institution");
        assert_eq!(stemmed("institutionalized"), "institution");
    }

    #[test]
    fn short_and_pathological_tokens_are_safe() {
        assert_eq!(stemmed("is"), "is");
        assert_eq!(stemmed("ion"), "ion");
        let ys = "y".repeat(100_000);
        assert_eq!(stemmed(&ys).len(), ys.len());
    }
}
