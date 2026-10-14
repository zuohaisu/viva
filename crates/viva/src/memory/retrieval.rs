//! Hybrid FTS + Jaccard + HRR reranking (minimal port of upstream retrieval.py).

use std::collections::HashSet;

use crate::memory::hrr::{self, HRR_DIM};

pub const FTS_WEIGHT: f64 = 0.4;
pub const JACCARD_WEIGHT: f64 = 0.3;
pub const HRR_WEIGHT: f64 = 0.3;

const PUNCT: &[char] = &[
    '.', ',', ';', ':', '!', '?', '"', '\'', '(', ')', '[', ']', '{', '}', '#', '@', '<', '>',
];
const FTS_STOPWORDS: &[&str] = &[
    "a",
    "about",
    "above",
    "after",
    "again",
    "all",
    "am",
    "an",
    "and",
    "any",
    "are",
    "as",
    "at",
    "be",
    "because",
    "been",
    "before",
    "being",
    "between",
    "both",
    "but",
    "by",
    "can",
    "could",
    "did",
    "do",
    "does",
    "doing",
    "don",
    "down",
    "during",
    "each",
    "few",
    "for",
    "from",
    "further",
    "had",
    "has",
    "have",
    "having",
    "he",
    "her",
    "here",
    "hers",
    "herself",
    "him",
    "himself",
    "his",
    "how",
    "i",
    "if",
    "in",
    "into",
    "is",
    "it",
    "its",
    "itself",
    "just",
    "me",
    "more",
    "most",
    "my",
    "myself",
    "no",
    "nor",
    "not",
    "now",
    "of",
    "off",
    "on",
    "once",
    "only",
    "or",
    "other",
    "our",
    "ours",
    "ourselves",
    "out",
    "over",
    "own",
    "same",
    "she",
    "should",
    "so",
    "some",
    "such",
    "than",
    "that",
    "the",
    "their",
    "theirs",
    "them",
    "themselves",
    "then",
    "there",
    "these",
    "they",
    "this",
    "those",
    "through",
    "to",
    "too",
    "under",
    "until",
    "up",
    "very",
    "was",
    "we",
    "were",
    "what",
    "when",
    "where",
    "which",
    "while",
    "who",
    "whom",
    "why",
    "will",
    "with",
    "would",
    "you",
    "your",
    "yours",
    "yourself",
    "yourselves",
];

#[derive(Debug, Clone)]
pub struct CandidateFact {
    pub fact_id: String,
    pub content: String,
    #[allow(dead_code)]
    pub category: String,
    pub tags: String,
    pub trust_score: f64,
    pub hrr_vector: Option<Vec<u8>>,
    pub fts_rank: f64,
}

pub fn shift_similarity(sim: f64) -> f64 {
    (sim + 1.0) / 2.0
}

pub fn tokenize(text: &str) -> HashSet<String> {
    text.to_lowercase()
        .split_whitespace()
        .map(|w| w.trim_matches(PUNCT).to_string())
        .filter(|c| !c.is_empty())
        .collect()
}

pub fn jaccard(a: &HashSet<String>, b: &HashSet<String>) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let inter = a.intersection(b).count() as f64;
    let union = a.union(b).count() as f64;
    inter / union
}

pub fn sanitize_fts_query(query: &str) -> String {
    let mut tokens = Vec::new();
    for raw in query.to_lowercase().split_whitespace() {
        let c: String = raw
            .trim_matches(PUNCT)
            .chars()
            .filter(|ch| !matches!(ch, '"' | '(' | ')' | '*' | '^' | ':' | '-' | '+'))
            .collect();
        if c.len() >= 2 && !FTS_STOPWORDS.contains(&c.as_str()) {
            tokens.push(format!("\"{c}\""));
        }
    }
    if tokens.is_empty() {
        query.to_string()
    } else {
        tokens.join(" OR ")
    }
}

pub fn rerank(
    query: &str,
    mut candidates: Vec<CandidateFact>,
    limit: usize,
) -> Vec<(CandidateFact, f64)> {
    let query_tokens = tokenize(query);
    let mut query_vec: Option<Vec<f64>> = None;
    for fact in &mut candidates {
        let j = jaccard(
            &query_tokens,
            &tokenize(&fact.content)
                .union(&tokenize(&fact.tags))
                .cloned()
                .collect(),
        );
        let mut hrr_sim = 0.5;
        if HRR_WEIGHT > 0.0 {
            if let Some(blob) = &fact.hrr_vector {
                if let Ok(fact_vec) = hrr::bytes_to_phases(blob, HRR_DIM) {
                    if query_vec.is_none() {
                        query_vec = Some(hrr::encode_text(query, HRR_DIM));
                    }
                    if let Some(qv) = &query_vec {
                        hrr_sim = shift_similarity(hrr::similarity(qv, &fact_vec));
                    }
                }
            }
        }
        let relevance = FTS_WEIGHT * fact.fts_rank + JACCARD_WEIGHT * j + HRR_WEIGHT * hrr_sim;
        let score = relevance * fact.trust_score;
        fact.fts_rank = score;
    }
    candidates.sort_by(|a, b| {
        b.fts_rank
            .partial_cmp(&a.fts_rank)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    candidates
        .into_iter()
        .take(limit)
        .map(|f| {
            let score = f.fts_rank;
            (f, score)
        })
        .collect()
}

pub fn probe_score(entity: &str, fact: &CandidateFact) -> Option<f64> {
    let blob = fact.hrr_vector.as_ref()?;
    let fact_vec = hrr::bytes_to_phases(blob, HRR_DIM).ok()?;
    let probe_key = hrr::bind(
        &hrr::encode_atom(&entity.to_lowercase(), HRR_DIM),
        &hrr::encode_atom(hrr::ROLE_ENTITY, HRR_DIM),
    );
    let role_content = hrr::encode_atom(hrr::ROLE_CONTENT, HRR_DIM);
    let extracted = hrr::unbind(&fact_vec, &probe_key);
    let content_bound = hrr::bind(&hrr::encode_text(&fact.content, HRR_DIM), &role_content);
    Some(hrr::similarity(&extracted, &content_bound))
}
