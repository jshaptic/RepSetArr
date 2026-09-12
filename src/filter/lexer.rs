//! Tokenizer for filter expressions.
//!
//! Deliberately word operators. The set language next door uses `& | ^ - +`,
//! and having the two read differently is what keeps `a - b` (set difference)
//! from being confused with `a and not b` (filter conjunction).

use crate::expr::ParseError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Token {
    Name(String),
    And,
    Or,
    Not,
    LParen,
    RParen,
}

impl Token {
    pub fn describe(&self) -> String {
        match self {
            Token::Name(name) => format!("`{name}`"),
            Token::And => "`and`".into(),
            Token::Or => "`or`".into(),
            Token::Not => "`not`".into(),
            Token::LParen => "`(`".into(),
            Token::RParen => "`)`".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spanned {
    pub token: Token,
    /// Zero-based character offset, reported to the user as a column.
    pub pos: usize,
}

/// No operator is punctuation here, so a filter name may contain `-` bare.
fn is_name_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | ':' | '/')
}

pub fn lex(input: &str) -> Result<Vec<Spanned>, ParseError> {
    let chars: Vec<char> = input.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;

    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }

        let start = i;
        let token = match c {
            '(' => {
                i += 1;
                Token::LParen
            }
            ')' => {
                i += 1;
                Token::RParen
            }
            '!' => {
                i += 1;
                Token::Not
            }
            '&' | '|' => {
                // `&&` and `||` are accepted; a single one is almost certainly
                // someone reaching for the set language by mistake.
                if chars.get(i + 1) == Some(&c) {
                    i += 2;
                    if c == '&' { Token::And } else { Token::Or }
                } else {
                    let word = if c == '&' { "and" } else { "or" };
                    return Err(ParseError::new(
                        format!(
                            "`{c}` is a set operator; a filter expression uses `{word}` \
                             (or `{c}{c}`)"
                        ),
                        start,
                    ));
                }
            }
            '"' | '\'' => {
                let quote = c;
                i += 1;
                let mut name = String::new();
                loop {
                    match chars.get(i) {
                        None => {
                            return Err(ParseError::new(
                                format!("unterminated quoted name, expected a closing {quote}"),
                                start,
                            ));
                        }
                        Some(&ch) if ch == quote => {
                            i += 1;
                            break;
                        }
                        Some(&ch) => {
                            name.push(ch);
                            i += 1;
                        }
                    }
                }
                if name.is_empty() {
                    return Err(ParseError::new("empty quoted name", start));
                }
                // Quoting suppresses keyword recognition, so a filter really
                // called `not` is still reachable.
                Token::Name(name)
            }
            c if is_name_char(c) => {
                let mut name = String::new();
                while let Some(&ch) = chars.get(i) {
                    if !is_name_char(ch) {
                        break;
                    }
                    name.push(ch);
                    i += 1;
                }
                match name.to_ascii_lowercase().as_str() {
                    "and" => Token::And,
                    "or" => Token::Or,
                    "not" => Token::Not,
                    _ => Token::Name(name),
                }
            }
            other => {
                return Err(ParseError::new(
                    format!("unexpected character `{other}`"),
                    start,
                ));
            }
        };

        out.push(Spanned { token, pos: start });
    }

    Ok(out)
}
