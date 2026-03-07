//! Canonical A2L text emission and string escaping.
//! [`WriterOptions`] controls indentation, column alignment, and A2ML output.

use crate::error::Result;

#[derive(Debug, Clone)]
pub struct WriterOptions {
    pub columns: usize,
    pub write_a2ml: bool,
    pub indent: String,
}

impl Default for WriterOptions {
    fn default() -> Self {
        WriterOptions {
            columns: 3,
            write_a2ml: true,
            indent: "  ".to_string(),
        }
    }
}

fn needs_escape(c: char) -> bool {
    matches!(c, '\\' | '"' | '\n' | '\r' | '\t')
}

pub fn escape_str(input: &str) -> String {
    if !input.chars().any(needs_escape) {
        return input.to_string();
    }
    let mut out = String::with_capacity(input.len() + 8);
    for c in input.chars() {
        match c {
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            _ => out.push(c),
        }
    }
    out
}

pub fn unescape_str(input: &str) -> String {
    if !input.contains('\\') {
        return input.to_string();
    }
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some('t') => out.push('\t'),
                Some(other) => out.push(other),
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[derive(Debug)]
pub struct Writer {
    buf: String,
    depth: usize,
    pub opts: WriterOptions,
}

impl Writer {
    pub fn new(opts: WriterOptions) -> Self {
        Writer {
            buf: String::new(),
            depth: 0,
            opts,
        }
    }

    pub fn into_string(self) -> String {
        self.buf
    }

    fn indent(&self) -> String {
        self.opts.indent.repeat(self.depth)
    }

    pub fn blank_line(&mut self) {
        self.buf.push('\n');
    }

    pub fn tag_value(&mut self, tag: Option<&str>, value: Option<&str>, quotes: bool) {
        let value = match (value, quotes) {
            (None, true) => "",
            (None, false) => return,
            (Some(v), _) => v,
        };
        let indent = self.indent();
        let v = if quotes {
            escape_str(value)
        } else {
            value.to_string()
        };
        let q = if quotes { "\"" } else { "" };
        match tag {
            Some(t) => self
                .buf
                .push_str(&format!("{}{} {}{}{}\n", indent, t, q, v, q)),
            None => self.buf.push_str(&format!("{}{}{}{}\n", indent, q, v, q)),
        }
    }

    pub fn value_line(&mut self, tag: Option<&str>, value: &str) {
        let indent = self.indent();
        match tag {
            Some(t) => self.buf.push_str(&format!("{}{} {}\n", indent, t, value)),
            None => self.buf.push_str(&format!("{}{}\n", indent, value)),
        }
    }

    pub fn references<I, S>(&mut self, refs: I)
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let indent = self.indent();
        let mut line = String::from(&indent);
        let mut col = 0usize;
        let mut any = false;
        for r in refs {
            any = true;
            col += 1;
            if col.is_multiple_of(self.opts.columns.max(1)) {
                line.push_str(r.as_ref());
                line.push('\n');
                line.push_str(&indent);
            } else {
                line.push_str(r.as_ref());
                line.push(' ');
            }
        }
        if any {
            let trimmed = line.trim_end();
            self.buf.push_str(trimmed);
            self.buf.push('\n');
        }
    }

    pub fn begin_block(&mut self, keyword: &str) {
        let indent = self.indent();
        self.buf
            .push_str(&format!("{}/begin {}\n", indent, keyword));
        self.depth += 1;
    }

    pub fn end_block(&mut self, keyword: &str) {
        self.depth = self.depth.saturating_sub(1);
        let indent = self.indent();
        self.buf.push_str(&format!("{}/end {}\n", indent, keyword));
    }

    pub fn block<T: crate::node::Node>(&mut self, node: &T) -> Result<()> {
        self.begin_block(T::KEYWORD);
        node.write_body(self)?;
        self.end_block(T::KEYWORD);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_roundtrip() {
        let src = "a\"b\\c\nd\te\rf";
        let esc = escape_str(src);
        assert_eq!(esc, "a\\\"b\\\\c\\nd\\te\\rf");
        assert_eq!(unescape_str(&esc), src);
    }

    #[test]
    fn tag_value_formats() {
        let mut w = Writer::new(WriterOptions::default());
        w.tag_value(Some("ANNOTATION_LABEL"), Some("hi \"x\""), true);
        w.tag_value(Some("ECU_ADDRESS"), Some("0x100"), false);
        w.tag_value(Some("SKIPPED"), None, false);
        assert_eq!(
            w.into_string(),
            "ANNOTATION_LABEL \"hi \\\"x\\\"\"\nECU_ADDRESS 0x100\n"
        );
    }
}
