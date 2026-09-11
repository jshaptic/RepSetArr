//! The list expression language: `(trending | top250) - my_radarr - never`.

pub mod ast;
pub mod eval;
pub mod lexer;
pub mod parser;

pub use ast::{Expr, SetOp};
pub use eval::{Set, UnknownName, apply, eval};
pub use parser::parse;

/// A syntax error, carrying the character offset it was found at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub message: String,
    pub pos: usize,
}

impl ParseError {
    pub fn new(message: impl Into<String>, pos: usize) -> Self {
        ParseError {
            message: message.into(),
            pos,
        }
    }
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} (column {})", self.message, self.pos + 1)
    }
}

impl std::error::Error for ParseError {}
