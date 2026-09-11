//! Precedence-climbing parser for list expressions.
//!
//! Precedence follows Python's set operators, so anyone who has written
//! `a | b - c` in Python gets the same grouping here: `-` and `+` bind tightest,
//! then `&`, then `^`, then `|`. Bracket anything non-obvious.

use super::ParseError;
use super::ast::{Expr, SetOp};
use super::lexer::{Spanned, Token, lex};

struct Parser {
    tokens: Vec<Spanned>,
    pos: usize,
    /// Offset just past the last token, for "unexpected end of expression".
    end: usize,
}

fn binding_power(token: &Token) -> Option<(SetOp, u8)> {
    match token {
        Token::Minus => Some((SetOp::Difference, 4)),
        Token::Plus => Some((SetOp::Union, 4)),
        Token::Amp => Some((SetOp::Intersection, 3)),
        Token::Caret => Some((SetOp::SymmetricDifference, 2)),
        Token::Pipe => Some((SetOp::Union, 1)),
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

    fn parse_expr(&mut self, min_bp: u8) -> Result<Expr, ParseError> {
        let mut lhs = self.parse_atom()?;

        while let Some(spanned) = self.peek() {
            let Some((op, bp)) = binding_power(&spanned.token) else {
                break;
            };
            if bp < min_bp {
                break;
            }
            self.next();
            // Left associative: the right side may only bind operators that are
            // strictly tighter than this one.
            let rhs = self.parse_expr(bp + 1)?;
            lhs = Expr::binary(op, lhs, rhs);
        }

        Ok(lhs)
    }

    fn parse_atom(&mut self) -> Result<Expr, ParseError> {
        let position = self.position();
        let Some(spanned) = self.next() else {
            return Err(ParseError::new(
                "unexpected end of expression, expected a list name",
                position,
            ));
        };

        match spanned.token {
            Token::Name(name) => Ok(Expr::Name(name)),
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
                format!("expected a list name but found {}", other.describe()),
                spanned.pos,
            )),
        }
    }
}

pub fn parse(input: &str) -> Result<Expr, ParseError> {
    let tokens = lex(input)?;
    if tokens.is_empty() {
        return Err(ParseError::new("expression is empty", 0));
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
        assert_eq!(canonical("trending"), "trending");
    }

    #[test]
    fn operators_are_left_associative() {
        assert_eq!(canonical("a - b - c"), "((a - b) - c)");
        assert_eq!(canonical("a | b | c"), "((a | b) | c)");
    }

    #[test]
    fn precedence_follows_pythons_set_operators() {
        assert_eq!(canonical("a | b - c"), "(a | (b - c))");
        assert_eq!(canonical("a | b & c"), "(a | (b & c))");
        assert_eq!(canonical("a ^ b & c"), "(a ^ (b & c))");
        assert_eq!(canonical("a | b ^ c"), "(a | (b ^ c))");
        assert_eq!(canonical("a & b - c"), "(a & (b - c))");
        assert_eq!(canonical("a + b - c"), "((a + b) - c)".replace('+', "|"));
    }

    #[test]
    fn brackets_override_precedence() {
        assert_eq!(canonical("(a | b) - c"), "((a | b) - c)");
        assert_eq!(canonical("((a))"), "a");
        assert_eq!(canonical("(a | b) & (c - d)"), "((a | b) & (c - d))");
    }

    #[test]
    fn names_may_be_quoted_when_they_contain_a_dash() {
        assert_eq!(canonical("\"top-250\" - seen"), "(top-250 - seen)");
        assert_eq!(canonical("'my list' | b"), "(my list | b)");
    }

    #[test]
    fn dotted_colonned_and_slashed_names_are_bare() {
        assert_eq!(canonical("mdb:user/list_1.v2"), "mdb:user/list_1.v2");
    }

    #[test]
    fn names_are_collected_in_order_without_duplicates() {
        let expr = parse("(a | b) - a & c").unwrap();
        assert_eq!(expr.names(), vec!["a", "b", "c"]);
    }

    #[test]
    fn errors_point_at_the_offending_column() {
        let err = parse("a - ").unwrap_err();
        assert_eq!(err.pos, 4);
        assert!(err.message.contains("unexpected end"), "{}", err.message);

        let err = parse("(a | b").unwrap_err();
        assert!(err.message.contains("unbalanced"), "{}", err.message);

        let err = parse("a | b)").unwrap_err();
        assert_eq!(err.pos, 5);

        let err = parse("a b").unwrap_err();
        assert_eq!(err.pos, 2);

        let err = parse("a $ b").unwrap_err();
        assert_eq!(err.pos, 2);
        assert!(err.message.contains('$'), "{}", err.message);

        assert!(parse("").is_err());
        assert!(parse("   ").is_err());
    }
}
