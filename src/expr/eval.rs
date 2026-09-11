//! Evaluation of a parsed expression over sets of group ids.
//!
//! Sets are [`IndexSet`]s so that ordering is meaningful and stable: the union
//! keeps the left side's order and appends what is new on the right, and the
//! other operators keep the left side's order. That means a source's own rank
//! order survives the algebra unless the list asks for an explicit sort.

use std::collections::HashMap;

use indexmap::IndexSet;

use super::ast::{Expr, SetOp};
use crate::identity::GroupId;

pub type Set = IndexSet<GroupId>;

#[derive(Debug, thiserror::Error)]
#[error("expression references `{0}`, which is not a known source or list")]
pub struct UnknownName(pub String);

pub fn eval(expr: &Expr, sets: &HashMap<String, Set>) -> Result<Set, UnknownName> {
    match expr {
        Expr::Name(name) => sets
            .get(name)
            .cloned()
            .ok_or_else(|| UnknownName(name.clone())),
        Expr::Op { op, lhs, rhs } => {
            let lhs = eval(lhs, sets)?;
            let rhs = eval(rhs, sets)?;
            Ok(apply(*op, &lhs, &rhs))
        }
    }
}

pub fn apply(op: SetOp, lhs: &Set, rhs: &Set) -> Set {
    match op {
        SetOp::Union => {
            let mut out = lhs.clone();
            out.extend(rhs.iter().copied());
            out
        }
        SetOp::Difference => lhs
            .iter()
            .filter(|id| !rhs.contains(*id))
            .copied()
            .collect(),
        SetOp::Intersection => lhs.iter().filter(|id| rhs.contains(*id)).copied().collect(),
        SetOp::SymmetricDifference => {
            let mut out: Set = lhs
                .iter()
                .filter(|id| !rhs.contains(*id))
                .copied()
                .collect();
            out.extend(rhs.iter().filter(|id| !lhs.contains(*id)).copied());
            out
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::expr::parse;

    fn set(ids: &[usize]) -> Set {
        ids.iter().copied().collect()
    }

    fn universe() -> HashMap<String, Set> {
        HashMap::from([
            ("a".to_string(), set(&[1, 2, 3])),
            ("b".to_string(), set(&[3, 4])),
            ("c".to_string(), set(&[2, 4, 5])),
            ("empty".to_string(), set(&[])),
        ])
    }

    fn run(expr: &str) -> Vec<usize> {
        eval(&parse(expr).unwrap(), &universe())
            .unwrap()
            .into_iter()
            .collect()
    }

    #[test]
    fn each_operator_does_what_it_says() {
        assert_eq!(run("a | b"), vec![1, 2, 3, 4]);
        assert_eq!(run("a + b"), vec![1, 2, 3, 4]);
        assert_eq!(run("a - b"), vec![1, 2]);
        assert_eq!(run("a & c"), vec![2]);
        assert_eq!(run("a ^ c"), vec![1, 3, 4, 5]);
    }

    #[test]
    fn union_keeps_the_left_order_and_appends_what_is_new() {
        assert_eq!(run("b | a"), vec![3, 4, 1, 2]);
    }

    #[test]
    fn multiple_operations_and_brackets_compose() {
        assert_eq!(run("(a | b) - c"), vec![1, 3]);
        assert_eq!(run("a | b - c"), vec![1, 2, 3]);
        assert_eq!(run("(a | b | c) - (a & c)"), vec![1, 3, 4, 5]);
    }

    #[test]
    fn empty_sides_behave() {
        assert_eq!(run("a - empty"), vec![1, 2, 3]);
        assert_eq!(run("empty | a"), vec![1, 2, 3]);
        assert_eq!(run("a & empty"), Vec::<usize>::new());
    }

    #[test]
    fn an_unknown_name_is_reported() {
        let err = eval(&parse("a | nope").unwrap(), &universe()).unwrap_err();
        assert_eq!(err.0, "nope");
    }
}
