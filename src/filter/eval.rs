//! Turning a parsed filter expression into something that can be run per item.
//!
//! Names are resolved once, at config-compile time, into a tree that holds the
//! filter blocks directly. Evaluation then allocates nothing and cannot fail,
//! which matters because it runs once per item per request.

use std::collections::BTreeSet;
use std::sync::Arc;

use indexmap::IndexMap;

use super::ast::BoolExpr;
use super::attr::Attribute;
use super::cond::FilterDef;
use crate::model::Item;

#[derive(Debug, thiserror::Error)]
#[error("filter expression references `{0}`, which is not a defined filter")]
pub struct UnknownFilter(pub String);

#[derive(Debug, Clone)]
pub enum Program {
    Leaf(Arc<FilterDef>),
    Not(Box<Program>),
    And(Box<Program>, Box<Program>),
    Or(Box<Program>, Box<Program>),
}

impl Program {
    pub fn compile(
        expr: &BoolExpr,
        filters: &IndexMap<String, Arc<FilterDef>>,
    ) -> Result<Program, UnknownFilter> {
        Ok(match expr {
            BoolExpr::Name(name) => Program::Leaf(
                filters
                    .get(name)
                    .cloned()
                    .ok_or_else(|| UnknownFilter(name.clone()))?,
            ),
            BoolExpr::Not(inner) => Program::Not(Box::new(Program::compile(inner, filters)?)),
            BoolExpr::And(lhs, rhs) => Program::And(
                Box::new(Program::compile(lhs, filters)?),
                Box::new(Program::compile(rhs, filters)?),
            ),
            BoolExpr::Or(lhs, rhs) => Program::Or(
                Box::new(Program::compile(lhs, filters)?),
                Box::new(Program::compile(rhs, filters)?),
            ),
        })
    }

    pub fn matches(&self, item: &Item) -> bool {
        match self {
            Program::Leaf(filter) => filter.matches(item),
            Program::Not(inner) => !inner.matches(item),
            Program::And(lhs, rhs) => lhs.matches(item) && rhs.matches(item),
            Program::Or(lhs, rhs) => lhs.matches(item) || rhs.matches(item),
        }
    }

    /// Every attribute this program reads. The enricher uses it to fetch only
    /// what is actually going to be looked at.
    pub fn attributes(&self) -> BTreeSet<Attribute> {
        let mut out = BTreeSet::new();
        self.collect_attributes(&mut out);
        out
    }

    fn collect_attributes(&self, out: &mut BTreeSet<Attribute>) {
        match self {
            Program::Leaf(filter) => out.extend(filter.attributes()),
            Program::Not(inner) => inner.collect_attributes(out),
            Program::And(lhs, rhs) | Program::Or(lhs, rhs) => {
                lhs.collect_attributes(out);
                rhs.collect_attributes(out);
            }
        }
    }
}
