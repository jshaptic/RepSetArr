//! Tokenizer for list expressions.

use super::ParseError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Token {
    Name(String),
    Pipe,
    Plus,
    Minus,
    Amp,
    Caret,
    LParen,
    RParen,
}

impl Token {
    pub fn describe(&self) -> String {
        match self {
            Token::Name(name) => format!("`{name}`"),
            Token::Pipe => "`|`".into(),
            Token::Plus => "`+`".into(),
            Token::Minus => "`-`".into(),
            Token::Amp => "`&`".into(),
            Token::Caret => "`^`".into(),
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

/// `-` is an operator, so it cannot also be a name character; a name that needs
/// one has to be quoted. Everything else an *Arr-style list name uses is fine.
///
/// `*` is here so that a wildcard pattern lexes as a single name; it is
/// reserved, and a configured name may not contain one.
fn is_name_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '.' | ':' | '/' | '*')
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
            '|' => {
                i += 1;
                Token::Pipe
            }
            '+' => {
                i += 1;
                Token::Plus
            }
            '-' => {
                i += 1;
                Token::Minus
            }
            '&' => {
                i += 1;
                Token::Amp
            }
            '^' => {
                i += 1;
                Token::Caret
            }
            '(' => {
                i += 1;
                Token::LParen
            }
            ')' => {
                i += 1;
                Token::RParen
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
                Token::Name(name)
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
