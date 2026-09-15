//! Wildcard patterns over configured names: `animation.studios.*`.
//!
//! A pattern stands for the union of every source and list whose name it
//! matches, so a formula keeps working as the config grows. Patterns are
//! expanded once, when the config is compiled - everything downstream
//! (dependency tracking, cycle detection, evaluation) only ever sees names.

use super::ast::{Expr, SetOp};

/// `*` is reserved: a configured name may not contain one, so a name that does
/// is always a pattern - quoting does not turn it back into a literal.
pub const WILDCARD: char = '*';

pub fn is_pattern(name: &str) -> bool {
    name.contains(WILDCARD)
}

/// `*` stands for any run of characters, dots included, so `animation.*` also
/// matches `animation.studios.ghibli`. Everything else is literal.
pub fn matches(pattern: &str, name: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let name: Vec<char> = name.chars().collect();
    let mut p = 0;
    let mut n = 0;
    // The last `*` we passed, and how much of the name it had consumed: when a
    // literal run fails further along we come back and let it eat one more.
    let mut star: Option<usize> = None;
    let mut consumed = 0;

    while n < name.len() {
        if p < pattern.len() && pattern[p] == WILDCARD {
            star = Some(p);
            consumed = n;
            p += 1;
        } else if p < pattern.len() && pattern[p] == name[n] {
            p += 1;
            n += 1;
        } else if let Some(position) = star {
            p = position + 1;
            consumed += 1;
            n = consumed;
        } else {
            return false;
        }
    }

    while p < pattern.len() && pattern[p] == WILDCARD {
        p += 1;
    }
    p == pattern.len()
}

/// Every candidate the pattern matches, in the order given, minus `defining` -
/// a list may not expand to include itself.
pub fn expand<'a>(
    pattern: &str,
    candidates: impl IntoIterator<Item = &'a str>,
    defining: &str,
) -> Vec<&'a str> {
    candidates
        .into_iter()
        .filter(|candidate| *candidate != defining && matches(pattern, candidate))
        .collect()
}

/// Replace every wildcard with a left-nested union of the names it matches.
///
/// Patterns that match nothing are returned in order of first appearance and
/// left in the tree unchanged, so the caller can report all of them at once
/// rather than one per restart.
pub fn expand_expr(expr: &Expr, candidates: &[&str], defining: &str) -> (Expr, Vec<String>) {
    let mut unmatched = Vec::new();
    let expanded = rewrite(expr, candidates, defining, &mut unmatched);
    (expanded, unmatched)
}

fn rewrite(expr: &Expr, candidates: &[&str], defining: &str, unmatched: &mut Vec<String>) -> Expr {
    match expr {
        Expr::Name(_) => expr.clone(),
        Expr::Op { op, lhs, rhs } => Expr::binary(
            *op,
            rewrite(lhs, candidates, defining, unmatched),
            rewrite(rhs, candidates, defining, unmatched),
        ),
        Expr::Wildcard(pattern) => {
            let matched = expand(pattern, candidates.iter().copied(), defining);
            match union_of(&matched) {
                Some(expanded) => expanded,
                None => {
                    if !unmatched.contains(pattern) {
                        unmatched.push(pattern.clone());
                    }
                    expr.clone()
                }
            }
        }
    }
}

/// Left-nested so the union keeps the candidates' order; a single match needs
/// no union at all.
fn union_of(names: &[&str]) -> Option<Expr> {
    let mut names = names.iter();
    let mut out = Expr::Name((*names.next()?).to_string());
    for name in names {
        out = Expr::binary(SetOp::Union, out, Expr::Name((*name).to_string()));
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::expr::parse;

    const UNIVERSE: [&str; 5] = [
        "animation.studios.ghibli",
        "animation.studios.disney",
        "animation.studios.pixar",
        "animation.studios.ghibli.classics",
        "live_action.nolan",
    ];

    fn expanded(formula: &str, defining: &str) -> (String, Vec<String>) {
        let (expr, unmatched) = expand_expr(&parse(formula).unwrap(), &UNIVERSE, defining);
        (expr.to_canonical_string(), unmatched)
    }

    #[test]
    fn a_star_stands_for_any_run_of_characters() {
        assert!(matches("a.*", "a.b"));
        assert!(matches("a.*", "a."));
        assert!(matches("a.*", "a.bbb"));
        assert!(!matches("a.*", "b.x"));
        assert!(!matches("a.*", "a"));
    }

    #[test]
    fn a_star_crosses_dots() {
        assert!(matches("animation.*", "animation.studios.ghibli"));
        assert!(matches("*", "anything.at.all"));
    }

    #[test]
    fn a_star_may_appear_anywhere_and_more_than_once() {
        assert!(matches("*.ghibli", "animation.studios.ghibli"));
        assert!(matches("animation.*.pixar", "animation.studios.pixar"));
        assert!(matches("*studios*", "animation.studios.ghibli"));
        assert!(matches("a**b", "ab"));
        assert!(!matches("*.ghibli", "animation.studios.ghibli.classics"));
    }

    #[test]
    fn everything_but_the_star_is_literal() {
        assert!(!matches("a.b", "axb"));
        assert!(!matches("a.*", "axb"));
    }

    #[test]
    fn a_pattern_expands_to_a_left_nested_union_in_candidate_order() {
        let (expr, unmatched) = expanded("animation.studios.*", "all");
        assert_eq!(
            expr,
            "(((animation.studios.ghibli | animation.studios.disney) \
             | animation.studios.pixar) | animation.studios.ghibli.classics)"
        );
        assert!(unmatched.is_empty());
    }

    #[test]
    fn a_single_match_needs_no_union() {
        assert_eq!(expanded("*.nolan", "all").0, "live_action.nolan");
    }

    #[test]
    fn a_pattern_never_includes_the_list_that_defines_it() {
        let (expr, unmatched) = expanded("*.nolan", "live_action.nolan");
        assert_eq!(expr, "*.nolan");
        assert_eq!(unmatched, vec!["*.nolan".to_string()]);
    }

    #[test]
    fn expansion_happens_wherever_the_pattern_sits() {
        let (expr, _) = expanded("live_action.nolan - (*.pixar | *.disney)", "all");
        assert_eq!(
            expr,
            "(live_action.nolan - (animation.studios.pixar | animation.studios.disney))"
        );
    }

    #[test]
    fn an_unmatched_pattern_is_reported_once_and_left_in_place() {
        let (expr, unmatched) = expanded("ghost.* | ghost.*", "all");
        assert_eq!(expr, "(ghost.* | ghost.*)");
        assert_eq!(unmatched, vec!["ghost.*".to_string()]);
    }

    #[test]
    fn a_name_the_pattern_also_matches_is_harmless() {
        // Union is idempotent, so overlapping with an explicit name is fine.
        let (expr, _) = expanded("*.pixar | animation.studios.pixar", "all");
        assert_eq!(expr, "(animation.studios.pixar | animation.studios.pixar)");
    }
}
