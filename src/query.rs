use std::io;

#[cfg(test)]
use grep_matcher::Matcher;
use grep_regex::{RegexMatcher, RegexMatcherBuilder};
use regex_syntax::hir::literal::{ExtractKind, Extractor, Seq};

pub struct Query {
    matcher: RegexMatcher,
    literal: Option<Vec<u8>>,
    plan: QueryPlan,
}

#[derive(Clone, Copy)]
enum QueryPlan {
    Literal,
    RegexPrefilter,
    RegexScan,
}

impl Query {
    pub fn build(pattern: &str, fixed_strings: bool, case_insensitive: bool) -> io::Result<Self> {
        let literal_fast_path = !case_insensitive && (fixed_strings || is_plain_literal(pattern));
        let mut builder = RegexMatcherBuilder::new();
        builder
            .case_insensitive(case_insensitive)
            .fixed_strings(fixed_strings || literal_fast_path)
            .line_terminator(Some(b'\n'));
        let matcher = builder
            .build(pattern)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
        let regex_literal = (!case_insensitive && !literal_fast_path)
            .then(|| extract_regex_literal(pattern))
            .flatten();
        let literal = literal_fast_path
            .then(|| pattern.as_bytes().to_vec())
            .or(regex_literal);
        let plan = if literal_fast_path {
            QueryPlan::Literal
        } else if literal.is_some() {
            QueryPlan::RegexPrefilter
        } else {
            QueryPlan::RegexScan
        };

        Ok(Self {
            matcher,
            literal,
            plan,
        })
    }

    pub fn index_literal(&self) -> Option<&[u8]> {
        self.literal.as_deref().filter(|literal| literal.len() >= 3)
    }

    pub fn matcher(&self) -> &RegexMatcher {
        &self.matcher
    }

    #[cfg(test)]
    fn matches(&self, line: &[u8]) -> bool {
        self.matcher.find(line).is_ok_and(|result| result.is_some())
    }

    pub fn strategy_name(&self, indexed: bool) -> &'static str {
        match (self.plan, indexed) {
            (QueryPlan::Literal, true) => "binary trigram index + ripgrep literal verification",
            (QueryPlan::RegexPrefilter, true) => {
                "regex mandatory-literal index + ripgrep regex verification"
            }
            (QueryPlan::RegexScan, true) => "parallel ripgrep regex scan",
            (QueryPlan::Literal, false) => "parallel ripgrep literal scan",
            (QueryPlan::RegexPrefilter | QueryPlan::RegexScan, false) => {
                "parallel ripgrep regex scan"
            }
        }
    }
}

fn extract_regex_literal(pattern: &str) -> Option<Vec<u8>> {
    let hir = regex_syntax::parse(pattern).ok()?;
    let prefix = Extractor::new().extract(&hir);
    let mut suffix_extractor = Extractor::new();
    suffix_extractor.kind(ExtractKind::Suffix);
    let suffix = suffix_extractor.extract(&hir);

    [common_literal(&prefix), common_literal(&suffix)]
        .into_iter()
        .flatten()
        .filter(|literal| literal.len() >= 3)
        .max_by_key(Vec::len)
}

fn common_literal(sequence: &Seq) -> Option<Vec<u8>> {
    let literals = sequence.literals()?;
    let first = literals.first()?.as_bytes();
    if literals.iter().any(|literal| literal.as_bytes().is_empty()) {
        return None;
    }

    for len in (3..=first.len()).rev() {
        for start in 0..=first.len() - len {
            let candidate = &first[start..start + len];
            if literals
                .iter()
                .all(|literal| contains(literal.as_bytes(), candidate))
            {
                return Some(candidate.to_vec());
            }
        }
    }
    None
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

fn is_plain_literal(pattern: &str) -> bool {
    !pattern.bytes().any(|byte| {
        matches!(
            byte,
            b'.' | b'^'
                | b'$'
                | b'*'
                | b'+'
                | b'?'
                | b'('
                | b')'
                | b'['
                | b']'
                | b'{'
                | b'}'
                | b'|'
                | b'\\'
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regex_alternation_never_uses_literal_index() {
        let query = Query::build("serde|rayon", false, false).unwrap();
        assert!(query.index_literal().is_none());
        assert!(query.matches(b"use serde::Serialize;"));
        assert!(query.matches(b"use rayon::prelude::*;"));
    }

    #[test]
    fn plain_patterns_use_literal_fast_path() {
        let query = Query::build("SearchOptions", false, false).unwrap();
        assert_eq!(query.index_literal(), Some(b"SearchOptions".as_slice()));
    }

    #[test]
    fn fixed_string_with_regex_characters_uses_literal_fast_path() {
        let query = Query::build("serde|rayon", true, false).unwrap();
        assert_eq!(query.index_literal(), Some(b"serde|rayon".as_slice()));
        assert!(query.matches(b"the literal serde|rayon is here"));
        assert!(!query.matches(b"serde is not the same as rayon"));
    }

    #[test]
    fn regex_with_mandatory_literal_uses_index_prefilter() {
        let query = Query::build(r"fn\s+SearchOptions", false, false).unwrap();
        assert_eq!(query.index_literal(), Some(b"SearchOptions".as_slice()));
        assert!(query.matches(b"fn   SearchOptions() {}"));
    }

    #[test]
    fn alternation_with_common_literal_uses_index_prefilter() {
        let query = Query::build("needle_one|needle_two", false, false).unwrap();
        assert_eq!(query.index_literal(), Some(b"needle_".as_slice()));
    }

    #[test]
    fn optional_literal_does_not_use_unsafe_prefilter() {
        let query = Query::build("(needle)?", false, false).unwrap();
        assert!(query.index_literal().is_none());
    }
}
