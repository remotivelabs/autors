use crate::error::{Error, Result};

#[derive(Debug, Clone, Eq)]
pub struct Token {
    pub text: String,
    pub line: u32,
    pub quoted: bool,
}

impl PartialEq for Token {
    fn eq(&self, other: &Self) -> bool {
        self.text == other.text && self.quoted == other.quoted
    }
}

impl Token {
    pub fn is_begin(&self) -> bool {
        !self.quoted && self.text.eq_ignore_ascii_case("/begin")
    }

    pub fn is_end(&self) -> bool {
        !self.quoted && self.text.eq_ignore_ascii_case("/end")
    }
}

pub fn tokenize(src: &str) -> Result<Vec<Token>> {
    let bytes = src.as_bytes();
    let mut tokens = Vec::new();
    let mut i = 0usize;
    let mut line = 1u32;

    while i < bytes.len() {
        let c = bytes[i];
        match c {
            b'\n' => {
                line += 1;
                i += 1;
            }
            b' ' | b'\t' | b'\r' => i += 1,
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                let start_line = line;
                i += 2;
                let mut closed = false;
                while i < bytes.len() {
                    if bytes[i] == b'\n' {
                        line += 1;
                    }
                    if bytes[i] == b'*' && bytes.get(i + 1) == Some(&b'/') {
                        i += 2;
                        closed = true;
                        break;
                    }
                    i += 1;
                }
                if !closed {
                    return Err(Error::parse(start_line, "unterminated block comment"));
                }
            }
            b'"' => {
                let tok_line = line;
                i += 1;
                let start = i;
                let mut end = None;
                while i < bytes.len() {
                    match bytes[i] {
                        b'\\' => i += 2,
                        b'"' => {
                            end = Some(i);
                            break;
                        }
                        b'\n' => {
                            line += 1;
                            i += 1;
                        }
                        _ => i += 1,
                    }
                }
                let end = end.ok_or_else(|| Error::parse(tok_line, "unterminated string"))?;
                tokens.push(Token {
                    text: src[start..end].to_string(),
                    line: tok_line,
                    quoted: true,
                });
                i = end + 1;
            }
            _ => {
                let tok_line = line;
                let start = i;
                while i < bytes.len() && !matches!(bytes[i], b' ' | b'\t' | b'\r' | b'\n' | b'"') {
                    i += 1;
                }
                tokens.push(Token {
                    text: src[start..i].to_string(),
                    line: tok_line,
                    quoted: false,
                });
            }
        }
    }
    Ok(tokens)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenizes_keywords_strings_and_comments() {
        let src = "/begin ANNOTATION // note\n ANNOTATION_LABEL \"a \\\"b\\\" c\" /* x */ ANNOTATION_ORIGIN \"o\" /end ANNOTATION";
        let toks = tokenize(src).unwrap();
        let texts: Vec<&str> = toks.iter().map(|t| t.text.as_str()).collect();
        assert_eq!(
            texts,
            [
                "/begin",
                "ANNOTATION",
                "ANNOTATION_LABEL",
                "a \\\"b\\\" c",
                "ANNOTATION_ORIGIN",
                "o",
                "/end",
                "ANNOTATION"
            ]
        );
        assert!(toks[3].quoted);
        assert!(!toks[0].quoted);
    }

    #[test]
    fn tracks_lines() {
        let toks = tokenize("A\nB\n\nC").unwrap();
        assert_eq!(toks[0].line, 1);
        assert_eq!(toks[1].line, 2);
        assert_eq!(toks[2].line, 4);
    }

    #[test]
    fn rejects_unterminated_string() {
        assert!(tokenize("\"abc").is_err());
    }
}
