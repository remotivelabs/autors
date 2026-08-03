use std::path::Path;

use crate::block::{build_block_tree_owned, Block};
use crate::error::{Error, Result};
use crate::model::base::NamedFields;
use crate::model::header::Header;
use crate::model::module::Module;
use crate::model::unsupported::UnsupportedNode;
use crate::node::Node;
use crate::params::ParamCursor;
use crate::token::tokenize;
use crate::writer::{Writer, WriterOptions};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Project {
    pub named: NamedFields,
    pub children: Vec<ProjectChild>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ProjectChild {
    /// `/begin HEADER`.
    Header(Header),
    /// `/begin MODULE`.
    Module(Module),
    Unsupported(UnsupportedNode),
}

impl ProjectChild {
    pub fn write_block(&self, w: &mut Writer) -> Result<()> {
        match self {
            ProjectChild::Header(n) => n.write_block(w),
            ProjectChild::Module(n) => n.write_block(w),
            ProjectChild::Unsupported(n) => n.write_block(w),
        }
    }
}

impl Project {
    pub fn parse_str(src: &str) -> Result<Self> {
        let tokens = tokenize(src)?;
        let root = build_block_tree_owned(tokens)?;
        let block = root
            .child("PROJECT")
            .ok_or_else(|| Error::parse(0, "no /begin PROJECT block found"))?;
        Project::parse(block)
    }

    pub fn parse_file(path: impl AsRef<Path>) -> Result<Self> {
        let src = std::fs::read_to_string(path)?;
        Project::parse_str(&src)
    }

    pub fn write_string(&self) -> Result<String> {
        self.write_string_with(&WriterOptions::default())
    }

    pub fn write_string_with(&self, opts: &WriterOptions) -> Result<String> {
        let mut w = Writer::new(opts.clone());
        self.write_block(&mut w)?;
        Ok(w.into_string())
    }

    pub fn save(&self, path: impl AsRef<Path>) -> Result<()> {
        std::fs::write(path, self.write_string()?)?;
        Ok(())
    }

    pub fn header(&self) -> Option<&Header> {
        self.children.iter().find_map(|c| match c {
            ProjectChild::Header(h) => Some(h),
            _ => None,
        })
    }

    pub fn modules(&self) -> impl Iterator<Item = &Module> {
        self.children.iter().filter_map(|c| match c {
            ProjectChild::Module(m) => Some(m),
            _ => None,
        })
    }
}

impl Node for Project {
    const KEYWORD: &'static str = "PROJECT";

    fn parse(block: &Block) -> Result<Self> {
        let mut cur = ParamCursor::new(block);
        let named = NamedFields {
            name: cur.ident()?,
            description: if cur.is_empty() {
                None
            } else {
                Some(cur.string()?)
            },
        };
        let children = block
            .children()
            .map(|b| match b.keyword.as_str() {
                "HEADER" => Header::parse(b).map(ProjectChild::Header),
                "MODULE" => Module::parse(b).map(ProjectChild::Module),
                _ => Ok(ProjectChild::Unsupported(UnsupportedNode::from_block(b))),
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Project { named, children })
    }

    fn write_body(&self, w: &mut Writer) -> Result<()> {
        w.tag_value(None, self.named.description.as_deref(), true);
        for child in &self.children {
            w.blank_line();
            child.write_block(w)?;
        }
        Ok(())
    }

    fn write_block(&self, w: &mut Writer) -> Result<()> {
        w.begin_block(Self::KEYWORD);
        w.tag_value(None, Some(&self.named.name), false);
        self.write_body(w)?;
        w.end_block(Self::KEYWORD);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"/begin PROJECT MyProj "demo project"

/begin HEADER
  "header comment"
  VERSION "1.71"
  PROJECT_NO 42
/end HEADER

/begin MODULE M1 "module one"
/end MODULE

/begin MODULE M2 "module two"
/end MODULE
/end PROJECT
"#;

    #[test]
    fn project_roundtrip() {
        let p = Project::parse_str(SAMPLE).unwrap();
        assert_eq!(p.named.name, "MyProj");
        assert_eq!(p.named.description.as_deref(), Some("demo project"));
        assert!(p.header().is_some());
        assert_eq!(p.modules().count(), 2);
        let out = p.write_string().unwrap();
        let p2 = Project::parse_str(&out).unwrap();
        assert_eq!(p, p2);
    }

    #[test]
    fn missing_project_is_error() {
        assert!(Project::parse_str("/begin MODULE M \"d\" /end MODULE").is_err());
    }
}
