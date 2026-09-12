//! Precedence-climbing parser for filter expressions.
//!
//! `not` binds tightest, then `and`, then `or` - the ordering every boolean
//! language uses, so `a or b and c` is `a or (b and c)`.

use super::ast::BoolExpr;
use super::lexer::{Spanned, Token, lex};
use crate::expr::ParseError;

struct Parser {
    tokens: Vec<Spanned>,
    pos: usize,
    /// Offset just past the last token, for "unexpected end of expression".
    end: usize,
}

fn binding_power(token: &Token) -> Option<u8> {
    match token {
        Token::And => Some(2),
        Token::Or => Some(1),
        _ => None,
    }
}

impl Parser {
    fn peek(&self) -> Option<&Spanned> {
        self.tokens.get(self.pos)
    }

    fn next(&mut self) -> Option<Spanned> {
        let item = self.tokens.get(self.pos).cloned();
        if item.is_some() {
            self.pos += 1;
        }
        item
    }

    fn position(&self) -> usize {
        self.peek().map(|s| s.pos).unwrap_or(self.end)
    }

    fn parse_expr(&mut self, min_bp: u8) -> Result<BoolExpr, ParseError> {
        let mut lhs = self.parse_atom()?;

        while let Some(spanned) = self.peek() {
            let Some(bp) = binding_power(&spanned.token) else {
                break;
            };
            if bp < min_bp {
                break;
            }
            let is_and = spanned.token == Token::And;
            self.next();
            // Left associative: the right side may only bind operators that are
            // strictly tighter than this one.
            let rhs = self.parse_expr(bp + 1)?;
            lhs = if is_and {
                BoolExpr::and(lhs, rhs)
            } else {
                BoolExpr::or(lhs, rhs)
            };
        }

        Ok(lhs)
    }

    fn parse_atom(&mut self) -> Result<BoolExpr, ParseError> {
        let position = self.position();
        let Some(spanned) = self.next() else {
            return Err(ParseError::new(
                "unexpected end of expression, expected a filter name",
                position,
            ));
        };

        match spanned.token {
            Token::Name(name) => Ok(BoolExpr::Name(name)),
            // Prefix, and tighter than any infix operator, so `not a and b`
            // means `(not a) and b`.
            Token::Not => Ok(BoolExpr::negate(self.parse_atom()?)),
            Token::LParen => {
                let inner = self.parse_expr(0)?;
                match self.next() {
                    Some(Spanned {
                        token: Token::RParen,
                        ..
                    }) => Ok(inner),
                    Some(other) => Err(ParseError::new(
                        format!("expected `)` but found {}", other.token.describe()),
                        other.pos,
                    )),
                    None => Err(ParseError::new(
                        "unbalanced brackets, expected `)`",
                        self.end,
                    )),
                }
            }
            other => Err(ParseError::new(
                format!("expected a filter name but found {}", other.describe()),
                spanned.pos,
            )),
        }
    }
}

pub fn parse(input: &str) -> Result<BoolExpr, ParseError> {
    let tokens = lex(input)?;
    if tokens.is_empty() {
        return Err(ParseError::new("filter expression is empty", 0));
    }
    let mut parser = Parser {
        end: input.chars().count(),
        tokens,
        pos: 0,
    };
    let expr = parser.parse_expr(0)?;
    if let Some(spanned) = parser.peek() {
        let message = if spanned.token == Token::RParen {
            "unbalanced brackets, unexpected `)`".to_string()
        } else {
            format!("unexpected trailing {}", spanned.token.describe())
        };
        return Err(ParseError::new(message, spanned.pos));
    }
    Ok(expr)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn canonical(input: &str) -> String {
        parse(input).expect("parses").to_canonical_string()
    }

    #[test]
    fn a_bare_name_parses() {
        assert_eq!(canonical("russian"), "russian");
    }

    #[test]
    fn and_binds_tighter_than_or() {
        assert_eq!(canonical("a or b and c"), "(a or (b and c))");
        assert_eq!(canonical("a and b or c"), "((a and b) or c)");
    }

    #[test]
    fn operators_are_left_associative() {
        assert_eq!(canonical("a and b and c"), "((a and b) and c)");
        assert_eq!(canonical("a or b or c"), "((a or b) or c)");
    }

    #[test]
    fn not_is_prefix_and_binds_tightest() {
        assert_eq!(canonical("not a and b"), "((not a) and b)");
        assert_eq!(canonical("not (a and b)"), "(not (a and b))");
        assert_eq!(canonical("not not a"), "(not (not a))");
    }

    #[test]
    fn brackets_override_precedence() {
        assert_eq!(canonical("(a or b) and c"), "((a or b) and c)");
        assert_eq!(canonical("((a))"), "a");
    }

    #[test]
    fn keywords_are_case_insensitive_and_symbols_are_accepted() {
        assert_eq!(canonical("a AND b"), "(a and b)");
        assert_eq!(canonical("a && b || c"), "((a and b) or c)");
        assert_eq!(canonical("!a"), "(not a)");
    }

    #[test]
    fn names_may_contain_dashes_and_be_quoted() {
        assert_eq!(
            canonical("kids-safe or animation"),
            "(kids-safe or animation)"
        );
        assert_eq!(canonical("'not' and a"), "(not and a)");
    }

    #[test]
    fn a_single_set_operator_explains_itself() {
        let err = parse("a & b").unwrap_err();
        assert_eq!(err.pos, 2);
        assert!(err.message.contains("`and`"), "{}", err.message);

        let err = parse("a | b").unwrap_err();
        assert!(err.message.contains("`or`"), "{}", err.message);
    }

    #[test]
    fn errors_point_at_the_offending_column() {
        let err = parse("a and ").unwrap_err();
        assert_eq!(err.pos, 6);
        assert!(err.message.contains("unexpected end"), "{}", err.message);

        assert!(parse("(a or b").unwrap_err().message.contains("unbalanced"));
        assert_eq!(parse("a or b)").unwrap_err().pos, 6);
        assert_eq!(parse("a b").unwrap_err().pos, 2);
        assert!(parse("").is_err());
        assert!(parse("   ").is_err());
    }

    #[test]
    fn names_are_collected_in_order_without_duplicates() {
        let expr = parse("(a or b) and not a and c").unwrap();
        assert_eq!(expr.names(), vec!["a", "b", "c"]);
    }
}
