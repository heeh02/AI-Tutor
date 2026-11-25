//! Pure, opt-in lexical candidate experiment. Not declared by lib.rs or used by
//! Store/context. Caller supplies source-valid, scope-eligible documents.
use regex::Regex;
use serde::Serialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::LazyLock,
};

pub const METHOD_VERSION: &str = "lexical-shadow-v1-20261104";
pub const FUNCTION_WORDS: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "been", "being", "by", "did", "do", "does", "for",
    "from", "had", "has", "have", "how", "i", "in", "is", "it", "its", "me", "my", "of", "on",
    "or", "our", "that", "the", "their", "them", "there", "these", "they", "this", "those", "to",
    "us", "was", "we", "were", "what", "which", "who", "whom", "whose", "why", "with", "would",
    "you", "your",
];
static TOKENS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
    r"(?P<latin>\p{Latin}+(?:['’]\p{Latin}+)*)|(?P<number>[+-]?[0-9]+(?:\.[0-9]+)?)|(?P<han>\p{Han}+)"
).expect("constant lexical feature pattern")
});

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FeatureKind {
    LatinWord,
    Number,
    HanBigram,
    HanSingleton,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct Feature {
    pub kind: FeatureKind,
    pub text: String,
}

/// All features retain token/run boundaries. This is a lexical representation,
/// not number interpretation, translation, or a claim about factual meaning.
pub fn features(text: &str) -> BTreeSet<Feature> {
    let mut result = BTreeSet::new();
    for capture in TOKENS.captures_iter(text) {
        if let Some(word) = capture.name("latin") {
            let word = word.as_str().to_lowercase().replace('’', "'");
            if !FUNCTION_WORDS.contains(&word.as_str()) {
                result.insert(Feature {
                    kind: FeatureKind::LatinWord,
                    text: word,
                });
            }
        } else if let Some(number) = capture.name("number") {
            let value = number.as_str();
            let preceded_by_alphanumeric = text[..number.start()]
                .chars()
                .next_back()
                .is_some_and(|previous| previous.is_alphanumeric());
            // A range/date/hyphenated word must not silently turn its next
            // component into a negative value. Literal signs require a boundary.
            let value =
                if preceded_by_alphanumeric && (value.starts_with('+') || value.starts_with('-')) {
                    &value[1..]
                } else {
                    value
                };
            result.insert(Feature {
                kind: FeatureKind::Number,
                text: value.into(),
            });
        } else if let Some(han) = capture.name("han") {
            let chars: Vec<_> = han.as_str().chars().collect();
            if chars.len() == 1 {
                result.insert(Feature {
                    kind: FeatureKind::HanSingleton,
                    text: han.as_str().into(),
                });
            } else {
                for pair in chars.windows(2) {
                    result.insert(Feature {
                        kind: FeatureKind::HanBigram,
                        text: pair.iter().collect(),
                    });
                }
            }
        }
    }
    result
}

pub struct Document<'a> {
    pub id: &'a str,
    pub text: &'a str,
}

#[derive(Debug, Serialize)]
pub struct MatchedFeature {
    pub feature: Feature,
    pub document_frequency: usize,
    pub idf: f64,
}

#[derive(Debug, Serialize)]
pub struct Candidate {
    pub id: String,
    pub score: f64,
    pub matched: Vec<MatchedFeature>,
    pub document_feature_count: usize,
    pub relevance_status: &'static str,
}

#[derive(Debug, Serialize)]
pub struct Ranking {
    pub query_features: BTreeSet<Feature>,
    pub eligible_document_count: usize,
    pub candidates: Vec<Candidate>,
    pub no_overlap_document_count: usize,
}

/// Returns every candidate with at least one exact feature match. The caller
/// applies its existing count/whole-record budget. Kind, grade and labels are
/// deliberately not inputs to this score; repeated terms cannot inflate it.
pub fn rank(query: &str, documents: &[Document<'_>]) -> Ranking {
    let query_features = features(query);
    let document_features: Vec<_> = documents
        .iter()
        .map(|document| features(document.text))
        .collect();
    let mut frequencies = BTreeMap::new();
    for terms in &document_features {
        for term in terms {
            *frequencies.entry(term.clone()).or_insert(0usize) += 1;
        }
    }
    let count = documents.len();
    let mut candidates = Vec::new();
    for (document, terms) in documents.iter().zip(&document_features) {
        let matched: Vec<_> = query_features
            .intersection(terms)
            .map(|feature| {
                let frequency = frequencies[feature];
                MatchedFeature {
                    feature: feature.clone(),
                    document_frequency: frequency,
                    idf: (1.0 + ((count - frequency) as f64 + 0.5) / (frequency as f64 + 0.5)).ln(),
                }
            })
            .collect();
        if !matched.is_empty() {
            candidates.push(Candidate {
                id: document.id.into(),
                score: matched.iter().map(|entry| entry.idf).sum(),
                matched,
                document_feature_count: terms.len(),
                relevance_status: "candidate_requires_relevance_review",
            });
        }
    }
    candidates.sort_by(|a, b| b.score.total_cmp(&a.score).then_with(|| a.id.cmp(&b.id)));
    Ranking {
        no_overlap_document_count: count - candidates.len(),
        query_features,
        eligible_document_count: count,
        candidates,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn term(kind: FeatureKind, text: &str) -> Feature {
        Feature {
            kind,
            text: text.into(),
        }
    }

    #[test]
    fn words_and_han_runs_never_cross_whitespace_punctuation_or_scripts() {
        let terms = features("RED blue; red-blue 停 车，停车甲A乙");
        assert!(terms.contains(&term(FeatureKind::LatinWord, "red")));
        assert!(terms.contains(&term(FeatureKind::LatinWord, "blue")));
        assert!(!terms.contains(&term(FeatureKind::LatinWord, "redblue")));
        assert!(!terms.contains(&term(FeatureKind::HanBigram, "甲乙")));
        assert!(!features("停 车，停。车").contains(&term(FeatureKind::HanBigram, "停车")));
        assert!(terms.contains(&term(FeatureKind::HanBigram, "停车")));
        assert!(
            rank(
                "painting",
                &[Document {
                    id: "part",
                    text: "paint"
                }]
            )
            .candidates
            .is_empty()
        );
    }

    #[test]
    fn numeric_spelling_sign_decimal_and_units_have_separate_boundaries() {
        let terms = features("20cm 30厘米 -20 +20 20.5 20,30 20-30 020");
        for number in ["20", "30", "-20", "+20", "20.5", "020"] {
            assert!(terms.contains(&term(FeatureKind::Number, number)));
        }
        assert!(!features("20-30").contains(&term(FeatureKind::Number, "-30")));
        assert!(!features("20 30").contains(&term(FeatureKind::Number, "2030")));
        assert!(
            rank(
                "20",
                &[Document {
                    id: "different",
                    text: "30"
                }]
            )
            .candidates
            .is_empty()
        );
    }

    #[test]
    fn negation_and_contractions_survive_function_word_filtering() {
        let terms = features("NOT no never neither nor without cannot CAN'T don’t isn't 0");
        for word in [
            "not", "no", "never", "neither", "nor", "without", "cannot", "can't", "don't", "isn't",
        ] {
            assert!(terms.contains(&term(FeatureKind::LatinWord, word)));
        }
        assert!(features("the and is of what").is_empty());
    }

    #[test]
    fn duplicates_do_not_inflate_df_or_scores_and_empty_matches_are_not_abstention() {
        let documents = [
            Document {
                id: "b",
                text: "alpha alpha",
            },
            Document {
                id: "a",
                text: "alpha",
            },
            Document {
                id: "c",
                text: "beta",
            },
        ];
        let ranking = rank("alpha alpha", &documents);
        assert_eq!(ranking.candidates.len(), 2);
        assert_eq!(ranking.candidates[0].id, "a");
        assert_eq!(ranking.candidates[0].score, ranking.candidates[1].score);
        assert_eq!(ranking.candidates[0].matched[0].document_frequency, 2);
        let expected = (1.0f64 + (3.0 - 2.0 + 0.5) / (2.0 + 0.5)).ln();
        assert!((ranking.candidates[0].score - expected).abs() < 1e-12);
        assert_eq!(
            ranking.candidates[0].relevance_status,
            "candidate_requires_relevance_review"
        );
        assert_eq!(rank("gamma", &documents).no_overlap_document_count, 3);
    }

    #[test]
    fn rarer_exact_features_receive_more_idf_without_domain_rules() {
        let documents = [
            Document {
                id: "a",
                text: "common rare",
            },
            Document {
                id: "b",
                text: "common",
            },
            Document {
                id: "c",
                text: "common",
            },
        ];
        let ranking = rank("common rare", &documents);
        let matched = &ranking.candidates[0].matched;
        let rare = matched
            .iter()
            .find(|item| item.feature.text == "rare")
            .unwrap();
        let common = matched
            .iter()
            .find(|item| item.feature.text == "common")
            .unwrap();
        assert!(rare.idf > common.idf);
    }
}
