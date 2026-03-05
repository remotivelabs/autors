//! Typed, position-aware access to a block's parameter tokens.
//! [`ParamCursor`] centralizes required-value checks, numeric parsing, and A2L
//! string unescaping so model nodes report consistent parse errors.

use crate::block::Block;
use crate::error::{Error, Result};
use crate::token::Token;
use crate::writer::unescape_str;

pub struct ParamCursor<'a> {
    toks: Vec<&'a Token>,
    pos: usize,
    owner: &'a str,
}

impl<'a> ParamCursor<'a> {
    pub fn new(block: &'a Block) -> Self {
        ParamCursor {
            toks: block.params().collect(),
            pos: 0,
            owner: &block.keyword,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.pos >= self.toks.len()
    }

    pub fn remaining(&self) -> usize {
        self.toks.len().saturating_sub(self.pos)
    }

    pub fn peek(&self) -> Option<&'a Token> {
        self.toks.get(self.pos).copied()
    }

    pub fn next_token(&mut self) -> Result<&'a Token> {
        let t = self.toks.get(self.pos).copied().ok_or_else(|| {
            Error::parse(0, format!("{}: unexpected end of parameters", self.owner))
        })?;
        self.pos += 1;
        Ok(t)
    }

    pub fn take_if(&mut self, tag: &str) -> bool {
        match self.peek() {
            Some(t) if !t.quoted && t.text.eq_ignore_ascii_case(tag) => {
                self.pos += 1;
                true
            }
            _ => false,
        }
    }

    pub fn expect(&mut self, tag: &str) -> Result<()> {
        let t = self.next_token()?;
        if !t.quoted && t.text.eq_ignore_ascii_case(tag) {
            Ok(())
        } else {
            Err(Error::parse(
                t.line,
                format!("{}: expected {}, got {:?}", self.owner, tag, t.text),
            ))
        }
    }

    pub fn string(&mut self) -> Result<String> {
        let t = self.next_token()?;
        Ok(unescape_str(&t.text))
    }

    pub fn ident(&mut self) -> Result<String> {
        let t = self.next_token()?;
        Ok(t.text.clone())
    }

    pub fn uint<T: A2lUint>(&mut self) -> Result<T> {
        let t = self.next_token()?;
        T::parse_a2l(&t.text).ok_or_else(|| {
            Error::parse(
                t.line,
                format!(
                    "{}: expected unsigned integer, got {:?}",
                    self.owner, t.text
                ),
            )
        })
    }

    pub fn int<T: A2lInt>(&mut self) -> Result<T> {
        let t = self.next_token()?;
        T::parse_a2l(&t.text).ok_or_else(|| {
            Error::parse(
                t.line,
                format!("{}: expected integer, got {:?}", self.owner, t.text),
            )
        })
    }

    pub fn float(&mut self) -> Result<f64> {
        let t = self.next_token()?;
        t.text.parse::<f64>().map_err(|_| {
            Error::parse(
                t.line,
                format!("{}: expected number, got {:?}", self.owner, t.text),
            )
        })
    }
}

pub trait A2lUint: Sized {
    fn parse_a2l(s: &str) -> Option<Self>;
}

pub trait A2lInt: Sized {
    fn parse_a2l(s: &str) -> Option<Self>;
}

macro_rules! impl_uint {
    ($($t:ty),*) => {$(
        impl A2lUint for $t {
            fn parse_a2l(s: &str) -> Option<Self> {
                if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
                    <$t>::from_str_radix(hex, 16).ok()
                } else {
                    s.parse::<$t>().ok()
                }
            }
        }
    )*};
}

macro_rules! impl_int {
    ($($t:ty),*) => {$(
        impl A2lInt for $t {
            fn parse_a2l(s: &str) -> Option<Self> {
                if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
                    <$t>::from_str_radix(hex, 16).ok()
                } else {
                    s.parse::<$t>().ok()
                }
            }
        }
    )*};
}

impl_uint!(u8, u16, u32, u64, usize);
impl_int!(i8, i16, i32, i64);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::build_block_tree;
    use crate::token::tokenize;

    #[test]
    fn reads_typed_values() {
        let src = "/begin M Name \"desc\" 0x1F -3 2.5 /end M";
        let toks = tokenize(src).unwrap();
        let root = build_block_tree(&toks).unwrap();
        let block = root.child("M").unwrap();
        let mut cur = ParamCursor::new(block);
        assert_eq!(cur.ident().unwrap(), "Name");
        assert_eq!(cur.string().unwrap(), "desc");
        assert_eq!(cur.uint::<u32>().unwrap(), 31);
        assert_eq!(cur.int::<i32>().unwrap(), -3);
        assert!((cur.float().unwrap() - 2.5).abs() < f64::EPSILON);
        assert!(cur.is_empty());
    }
}
