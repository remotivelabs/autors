use crate::error::{Error, Result};
use crate::token::Token;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    Param(Token),
    Child(Block),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    pub keyword: String,
    pub line: u32,
    pub items: Vec<Item>,
}

impl Block {
    pub fn children(&self) -> impl Iterator<Item = &Block> {
        self.items.iter().filter_map(|it| match it {
            Item::Child(b) => Some(b),
            _ => None,
        })
    }

    pub fn child(&self, keyword: &str) -> Option<&Block> {
        self.children().find(|b| b.keyword == keyword)
    }

    pub fn children_named<'a>(&'a self, keyword: &'a str) -> impl Iterator<Item = &'a Block> {
        self.children().filter(move |b| b.keyword == keyword)
    }

    pub fn params(&self) -> impl Iterator<Item = &Token> {
        self.items.iter().filter_map(|it| match it {
            Item::Param(t) => Some(t),
            _ => None,
        })
    }
}

pub fn build_block_tree(tokens: &[Token]) -> Result<Block> {
    let mut pos = 0usize;
    let root = build_inner(tokens, &mut pos, None)?;
    if pos != tokens.len() {
        return Err(Error::parse(
            tokens[pos].line,
            format!(
                "unexpected token {:?} outside of any block",
                tokens[pos].text
            ),
        ));
    }
    Ok(root)
}

/// Builds a block tree while consuming the token vector.
///
/// Parsing a complete document should prefer this form because parameter
/// tokens can be moved into the tree instead of cloning every token and its
/// text allocation.
pub fn build_block_tree_owned(tokens: Vec<Token>) -> Result<Block> {
    build_inner_owned(&mut tokens.into_iter(), None)
}

fn build_inner_owned(
    tokens: &mut std::vec::IntoIter<Token>,
    enclosing: Option<(String, u32)>,
) -> Result<Block> {
    let is_root = enclosing.is_none();
    let (keyword, line) = enclosing.unwrap_or_default();
    let mut items = Vec::new();
    while let Some(token) = tokens.next() {
        if token.is_begin() {
            let begin_line = token.line;
            let keyword_token = tokens
                .next()
                .ok_or_else(|| Error::parse(begin_line, "expected keyword after /begin"))?;
            let child = build_inner_owned(tokens, Some((keyword_token.text, begin_line)))?;
            items.push(Item::Child(child));
        } else if token.is_end() {
            let end_line = token.line;
            let keyword_token = tokens
                .next()
                .ok_or_else(|| Error::parse(end_line, "expected keyword after /end"))?;
            if is_root {
                return Err(Error::parse(end_line, "/end without matching /begin"));
            }
            if !keyword_token.text.eq_ignore_ascii_case(&keyword) {
                return Err(Error::parse(
                    end_line,
                    format!(
                        "/end {} does not match /begin {}",
                        keyword_token.text, keyword
                    ),
                ));
            }
            return Ok(Block {
                keyword,
                line,
                items,
            });
        } else {
            items.push(Item::Param(token));
        }
    }
    if is_root {
        Ok(Block {
            keyword,
            line,
            items,
        })
    } else {
        Err(Error::parse(line, format!("missing /end {keyword}")))
    }
}

fn build_inner(tokens: &[Token], pos: &mut usize, enclosing: Option<&str>) -> Result<Block> {
    let (kw, line) = match enclosing {
        Some(k) => (k.to_string(), tokens[*pos - 2].line),
        None => (String::new(), 0),
    };
    let mut items = Vec::new();
    while *pos < tokens.len() {
        let t = &tokens[*pos];
        if t.is_begin() {
            let begin_line = t.line;
            *pos += 1;
            let kw_tok = tokens
                .get(*pos)
                .ok_or_else(|| Error::parse(begin_line, "expected keyword after /begin"))?;
            let child_kw = kw_tok.text.clone();
            *pos += 1;
            let mut child = build_inner(tokens, pos, Some(&child_kw))?;
            child.line = begin_line;
            items.push(Item::Child(child));
        } else if t.is_end() {
            let end_line = t.line;
            *pos += 1;
            let kw_tok = tokens
                .get(*pos)
                .ok_or_else(|| Error::parse(end_line, "expected keyword after /end"))?;
            match enclosing {
                Some(k) if kw_tok.text.eq_ignore_ascii_case(k) => {
                    *pos += 1;
                    return Ok(Block {
                        keyword: kw,
                        line,
                        items,
                    });
                }
                Some(k) => {
                    return Err(Error::parse(
                        end_line,
                        format!("/end {} does not match /begin {}", kw_tok.text, k),
                    ))
                }
                None => {
                    return Err(Error::parse(end_line, "/end without matching /begin"));
                }
            }
        } else {
            items.push(Item::Param(t.clone()));
            *pos += 1;
        }
    }
    match enclosing {
        Some(k) => Err(Error::parse(line, format!("missing /end {}", k))),
        None => Ok(Block {
            keyword: kw,
            line,
            items,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::token::tokenize;

    #[test]
    fn builds_nested_tree() {
        let src = "/begin PROJECT P \"d\" /begin MODULE M \"d\" /end MODULE /end PROJECT";
        let toks = tokenize(src).unwrap();
        let root = build_block_tree(&toks).unwrap();
        let proj = &root.children().next().unwrap();
        assert_eq!(proj.keyword, "PROJECT");
        assert_eq!(proj.params().count(), 2);
        assert_eq!(proj.child("MODULE").unwrap().params().count(), 2);
    }

    #[test]
    fn rejects_mismatched_end() {
        let toks = tokenize("/begin A /end B").unwrap();
        assert!(build_block_tree(&toks).is_err());
    }

    #[test]
    fn owned_builder_matches_borrowed_builder() {
        let tokens = tokenize("A /begin B C /begin D E /end D /end B F").unwrap();
        assert_eq!(
            build_block_tree_owned(tokens.clone()).unwrap(),
            build_block_tree(&tokens).unwrap()
        );
    }
}
