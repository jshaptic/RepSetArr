//! The filter language: named blocks of Kometa-style conditions, combined per
//! list with boolean algebra (`russian and (kids_safe or animation)`).
//!
//! Filters are deliberately *not* terms in the set expression. A set operand is
//! a finite collection of items; a filter is a predicate with no extent of its
//! own. Keeping them in two languages - `& | ^ - +` for sets, `and or not` for
//! filters - means neither has to be read in the other's terms.

pub mod ast;
pub mod attr;
pub mod cond;
pub mod eval;
pub mod lexer;
pub mod parser;

pub use ast::BoolExpr;
pub use attr::{Attribute, Reading};
pub use cond::{Condition, FilterDef, UnknownPolicy};
pub use eval::{Program, UnknownFilter};
pub use parser::parse;
