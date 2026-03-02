//! `A2LREF_CHARACTERISTIC` / `A2LREF_GROUP` / `A2LREF_MEASUREMENT` /
//! `A2LDEF_CHARACTERISTIC` / `A2LDEPENDENT_CHARACTERISTIC` / `A2LIN_MEASUREMENT` /
//! `A2LLOC_MEASUREMENT` / `A2LOUT_MEASUREMENT` / `A2LSUB_FUNCTION` / `A2LSUB_GROUP` /
//! `A2LVIRTUAL` / `A2LVIRTUAL_CHARACTERISTIC` / `A2LFRAME`.

use crate::block::Block;
use crate::error::Result;
use crate::model::annotation::Annotation;
use crate::model::base::{NamedFields, ReferenceFields};
use crate::model::unsupported::UnsupportedNode;
use crate::node::Node;
use crate::params::ParamCursor;
use crate::writer::Writer;

fn write_named_block<T: Node>(node: &T, name: &str, w: &mut Writer) -> Result<()> {
    w.begin_block(&format!("{} {name}", T::KEYWORD));
    node.write_body(w)?;
    w.end_block(T::KEYWORD);
    Ok(())
}

macro_rules! ref_node {
    ($(#[$meta:meta])* $name:ident, $kw:literal) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Default, PartialEq, Eq)]
        pub struct $name {
            pub references: ReferenceFields,
        }

        impl Node for $name {
            const KEYWORD: &'static str = $kw;

            fn parse(block: &Block) -> Result<Self> {
                let mut cur = ParamCursor::new(block);
                let mut references = ReferenceFields::default();
                while !cur.is_empty() {
                    references.take(&mut cur)?;
                }
                Ok(Self { references })
            }

            fn write_body(&self, w: &mut Writer) -> Result<()> {
                self.references.write(w)
            }
        }
    };
}

ref_node!(DefCharacteristic, "DEF_CHARACTERISTIC");

ref_node!(FunctionList, "FUNCTION_LIST");

ref_node!(InMeasurement, "IN_MEASUREMENT");

ref_node!(LocMeasurement, "LOC_MEASUREMENT");

ref_node!(OutMeasurement, "OUT_MEASUREMENT");

ref_node!(RefCharacteristic, "REF_CHARACTERISTIC");

ref_node!(RefGroup, "REF_GROUP");

ref_node!(RefMeasurement, "REF_MEASUREMENT");

ref_node!(SubFunction, "SUB_FUNCTION");

ref_node!(SubGroup, "SUB_GROUP");

ref_node!(Virtual, "VIRTUAL");

fn parse_dependent(block: &Block) -> Result<(String, ReferenceFields)> {
    let mut cur = ParamCursor::new(block);
    let formula = cur.ident()?;
    let mut references = ReferenceFields::default();
    while !cur.is_empty() {
        references.take(&mut cur)?;
    }
    Ok((formula, references))
}

fn write_dependent(formula: &str, references: &ReferenceFields, w: &mut Writer) -> Result<()> {
    w.value_line(None, &format!("\"{formula}\""));
    references.write(w)
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DependentCharacteristic {
    pub formula: String,
    pub references: ReferenceFields,
}

impl Node for DependentCharacteristic {
    const KEYWORD: &'static str = "DEPENDENT_CHARACTERISTIC";

    fn parse(block: &Block) -> Result<Self> {
        let (formula, references) = parse_dependent(block)?;
        Ok(DependentCharacteristic {
            formula,
            references,
        })
    }

    fn write_body(&self, w: &mut Writer) -> Result<()> {
        write_dependent(&self.formula, &self.references, w)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VirtualCharacteristic {
    pub formula: String,
    pub references: ReferenceFields,
}

impl Node for VirtualCharacteristic {
    const KEYWORD: &'static str = "VIRTUAL_CHARACTERISTIC";

    fn parse(block: &Block) -> Result<Self> {
        let (formula, references) = parse_dependent(block)?;
        Ok(VirtualCharacteristic {
            formula,
            references,
        })
    }

    fn write_body(&self, w: &mut Writer) -> Result<()> {
        write_dependent(&self.formula, &self.references, w)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FunctionChild {
    /// `/begin ANNOTATION`.
    Annotation(Annotation),
    /// `/begin DEF_CHARACTERISTIC`.
    DefCharacteristic(DefCharacteristic),
    /// `/begin REF_CHARACTERISTIC`.
    RefCharacteristic(RefCharacteristic),
    /// `/begin IN_MEASUREMENT`.
    InMeasurement(InMeasurement),
    /// `/begin LOC_MEASUREMENT`.
    LocMeasurement(LocMeasurement),
    /// `/begin OUT_MEASUREMENT`.
    OutMeasurement(OutMeasurement),
    /// `/begin VIRTUAL_CHARACTERISTIC`.
    VirtualCharacteristic(VirtualCharacteristic),
    /// `/begin SUB_FUNCTION`.
    SubFunction(SubFunction),
    Unsupported(UnsupportedNode),
}

impl FunctionChild {
    pub fn write_block(&self, w: &mut Writer) -> Result<()> {
        match self {
            FunctionChild::Annotation(n) => n.write_block(w),
            FunctionChild::DefCharacteristic(n) => n.write_block(w),
            FunctionChild::RefCharacteristic(n) => n.write_block(w),
            FunctionChild::InMeasurement(n) => n.write_block(w),
            FunctionChild::LocMeasurement(n) => n.write_block(w),
            FunctionChild::OutMeasurement(n) => n.write_block(w),
            FunctionChild::VirtualCharacteristic(n) => n.write_block(w),
            FunctionChild::SubFunction(n) => n.write_block(w),
            FunctionChild::Unsupported(n) => n.write_block(w),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Function {
    pub named: NamedFields,
    pub version: Option<String>,
    pub children: Vec<FunctionChild>,
}

impl Node for Function {
    const KEYWORD: &'static str = "FUNCTION";

    fn parse(block: &Block) -> Result<Self> {
        let mut cur = ParamCursor::new(block);
        let named = NamedFields::read_header(&mut cur)?;
        let mut version = None;
        while !cur.is_empty() {
            if cur.take_if("FUNCTION_VERSION") {
                version = Some(cur.ident()?);
            } else {
                cur.next_token()?;
            }
        }
        let mut children = Vec::new();
        for child in block.children() {
            children.push(match child.keyword.to_ascii_uppercase().as_str() {
                "ANNOTATION" => FunctionChild::Annotation(Annotation::parse(child)?),
                "DEF_CHARACTERISTIC" => {
                    FunctionChild::DefCharacteristic(DefCharacteristic::parse(child)?)
                }
                "REF_CHARACTERISTIC" => {
                    FunctionChild::RefCharacteristic(RefCharacteristic::parse(child)?)
                }
                "IN_MEASUREMENT" => FunctionChild::InMeasurement(InMeasurement::parse(child)?),
                "LOC_MEASUREMENT" => FunctionChild::LocMeasurement(LocMeasurement::parse(child)?),
                "OUT_MEASUREMENT" => FunctionChild::OutMeasurement(OutMeasurement::parse(child)?),
                "VIRTUAL_CHARACTERISTIC" => {
                    FunctionChild::VirtualCharacteristic(VirtualCharacteristic::parse(child)?)
                }
                "SUB_FUNCTION" => FunctionChild::SubFunction(SubFunction::parse(child)?),
                _ => FunctionChild::Unsupported(UnsupportedNode::from_block(child)),
            });
        }
        Ok(Function {
            named,
            version,
            children,
        })
    }

    fn write_body(&self, w: &mut Writer) -> Result<()> {
        self.named.write(w)?;
        if let Some(v) = self.version.as_deref().filter(|s| !s.is_empty()) {
            w.value_line(None, &format!("FUNCTION_VERSION \"{v}\""));
        }
        for child in &self.children {
            child.write_block(w)?;
        }
        Ok(())
    }

    fn write_block(&self, w: &mut Writer) -> Result<()> {
        write_named_block(self, &self.named.name, w)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GroupChild {
    /// `/begin ANNOTATION`.
    Annotation(Annotation),
    /// `/begin REF_MEASUREMENT`.
    RefMeasurement(RefMeasurement),
    /// `/begin REF_CHARACTERISTIC`.
    RefCharacteristic(RefCharacteristic),
    /// `/begin FUNCTION_LIST`.
    FunctionList(FunctionList),
    /// `/begin SUB_GROUP`.
    SubGroup(SubGroup),
    Unsupported(UnsupportedNode),
}

impl GroupChild {
    pub fn write_block(&self, w: &mut Writer) -> Result<()> {
        match self {
            GroupChild::Annotation(n) => n.write_block(w),
            GroupChild::RefMeasurement(n) => n.write_block(w),
            GroupChild::RefCharacteristic(n) => n.write_block(w),
            GroupChild::FunctionList(n) => n.write_block(w),
            GroupChild::SubGroup(n) => n.write_block(w),
            GroupChild::Unsupported(n) => n.write_block(w),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Group {
    pub named: NamedFields,
    pub root: bool,
    pub children: Vec<GroupChild>,
}

impl Node for Group {
    const KEYWORD: &'static str = "GROUP";

    fn parse(block: &Block) -> Result<Self> {
        let mut cur = ParamCursor::new(block);
        let named = NamedFields::read_header(&mut cur)?;
        let mut root = false;
        while !cur.is_empty() {
            if cur.take_if("ROOT") {
                root = true;
            } else {
                cur.next_token()?;
            }
        }
        let mut children = Vec::new();
        for child in block.children() {
            children.push(match child.keyword.to_ascii_uppercase().as_str() {
                "ANNOTATION" => GroupChild::Annotation(Annotation::parse(child)?),
                "REF_MEASUREMENT" => GroupChild::RefMeasurement(RefMeasurement::parse(child)?),
                "REF_CHARACTERISTIC" => {
                    GroupChild::RefCharacteristic(RefCharacteristic::parse(child)?)
                }
                "FUNCTION_LIST" => GroupChild::FunctionList(FunctionList::parse(child)?),
                "SUB_GROUP" => GroupChild::SubGroup(SubGroup::parse(child)?),
                _ => GroupChild::Unsupported(UnsupportedNode::from_block(child)),
            });
        }
        Ok(Group {
            named,
            root,
            children,
        })
    }

    fn write_body(&self, w: &mut Writer) -> Result<()> {
        self.named.write(w)?;
        if self.root {
            w.value_line(None, "ROOT");
        }
        for child in &self.children {
            child.write_block(w)?;
        }
        Ok(())
    }

    fn write_block(&self, w: &mut Writer) -> Result<()> {
        write_named_block(self, &self.named.name, w)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Frame {
    pub named: NamedFields,
    pub scaling_unit: i32,
    pub rate: i64,
    pub frame_measurements: Option<Vec<String>>,
    pub children: Vec<UnsupportedNode>,
}

impl Node for Frame {
    const KEYWORD: &'static str = "FRAME";

    fn parse(block: &Block) -> Result<Self> {
        let mut cur = ParamCursor::new(block);
        let named = NamedFields::read_header(&mut cur)?;
        let scaling_unit = cur.int::<i32>()?;
        let rate = cur.int::<i64>()?;
        let mut frame_measurements = None;
        while !cur.is_empty() {
            if cur.take_if("FRAME_MEASUREMENT") {
                let mut refs = Vec::new();
                while !cur.is_empty() {
                    refs.push(cur.ident()?);
                }
                frame_measurements = Some(refs);
            } else {
                cur.next_token()?;
            }
        }
        let children = block.children().map(UnsupportedNode::from_block).collect();
        Ok(Frame {
            named,
            scaling_unit,
            rate,
            frame_measurements,
            children,
        })
    }

    fn write_body(&self, w: &mut Writer) -> Result<()> {
        self.named.write(w)?;
        w.value_line(None, &self.scaling_unit.to_string());
        w.value_line(None, &self.rate.to_string());
        if let Some(refs) = &self.frame_measurements {
            for (i, r) in refs.iter().enumerate() {
                if i == 0 {
                    w.value_line(Some("FRAME_MEASUREMENT"), r);
                } else {
                    w.value_line(None, r);
                }
            }
        }
        for child in &self.children {
            child.write_block(w)?;
        }
        Ok(())
    }

    fn write_block(&self, w: &mut Writer) -> Result<()> {
        write_named_block(self, &self.named.name, w)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::build_block_tree;
    use crate::token::tokenize;
    use crate::writer::WriterOptions;

    fn roundtrip<T: Node>(src: &str) -> String {
        let toks = tokenize(src).unwrap();
        let root = build_block_tree(&toks).unwrap();
        let block = root.child(T::KEYWORD).unwrap();
        let node = T::parse(block).unwrap();
        let mut w = Writer::new(WriterOptions::default());
        node.write_block(&mut w).unwrap();
        w.into_string()
    }

    #[test]
    fn function_full() {
        let src = "/begin FUNCTION F1 \"func desc\" FUNCTION_VERSION \"1.2\" \
                   /begin DEF_CHARACTERISTIC C1 C2 /end DEF_CHARACTERISTIC \
                   /begin REF_CHARACTERISTIC C3 C4 C5 C6 /end REF_CHARACTERISTIC \
                   /begin IN_MEASUREMENT M1 /end IN_MEASUREMENT \
                   /begin LOC_MEASUREMENT M2 /end LOC_MEASUREMENT \
                   /begin OUT_MEASUREMENT M3 /end OUT_MEASUREMENT \
                   /begin VIRTUAL_CHARACTERISTIC \"f(x)\" VC1 /end VIRTUAL_CHARACTERISTIC \
                   /begin SUB_FUNCTION SF1 /end SUB_FUNCTION \
                   /begin ANNOTATION ANNOTATION_LABEL \"lbl\" /end ANNOTATION \
                   /begin IF_DATA XCP /end IF_DATA \
                   /end FUNCTION";
        let out = roundtrip::<Function>(src);
        assert_eq!(
            out,
            "/begin FUNCTION F1\n  \"func desc\"\n  FUNCTION_VERSION \"1.2\"\n  \
             /begin DEF_CHARACTERISTIC\n    C1 C2\n  /end DEF_CHARACTERISTIC\n  \
             /begin REF_CHARACTERISTIC\n    C3 C4 C5\n    C6\n  /end REF_CHARACTERISTIC\n  \
             /begin IN_MEASUREMENT\n    M1\n  /end IN_MEASUREMENT\n  \
             /begin LOC_MEASUREMENT\n    M2\n  /end LOC_MEASUREMENT\n  \
             /begin OUT_MEASUREMENT\n    M3\n  /end OUT_MEASUREMENT\n  \
             /begin VIRTUAL_CHARACTERISTIC\n    \"f(x)\"\n    VC1\n  /end VIRTUAL_CHARACTERISTIC\n  \
             /begin SUB_FUNCTION\n    SF1\n  /end SUB_FUNCTION\n  \
             /begin ANNOTATION\n    ANNOTATION_LABEL \"lbl\"\n  /end ANNOTATION\n  \
             /begin IF_DATA\n    XCP\n  /end IF_DATA\n/end FUNCTION\n"
        );
        let toks = tokenize(&out).unwrap();
        let root = build_block_tree(&toks).unwrap();
        let f2 = Function::parse(root.child("FUNCTION").unwrap()).unwrap();
        assert_eq!(f2.named.name, "F1");
        assert_eq!(f2.version.as_deref(), Some("1.2"));
        assert_eq!(f2.children.len(), 9);
    }

    #[test]
    fn function_minimal_no_version() {
        let out = roundtrip::<Function>("/begin FUNCTION F1 \"\" /end FUNCTION");
        assert_eq!(out, "/begin FUNCTION F1\n  \"\"\n/end FUNCTION\n");
    }

    #[test]
    fn function_skips_unknown_flat() {
        let out = roundtrip::<Function>(
            "/begin FUNCTION F \"d\" FOO BAR FUNCTION_VERSION \"2\" /end FUNCTION",
        );
        assert_eq!(
            out,
            "/begin FUNCTION F\n  \"d\"\n  FUNCTION_VERSION \"2\"\n/end FUNCTION\n"
        );
    }

    #[test]
    fn group_full() {
        let src = "/begin GROUP G1 \"grp desc\" ROOT \
                   /begin REF_MEASUREMENT M1 M2 M3 M4 /end REF_MEASUREMENT \
                   /begin REF_CHARACTERISTIC C1 /end REF_CHARACTERISTIC \
                   /begin FUNCTION_LIST F1 F2 /end FUNCTION_LIST \
                   /begin SUB_GROUP G2 /end SUB_GROUP \
                   /begin ANNOTATION ANNOTATION_ORIGIN \"org\" /end ANNOTATION \
                   /begin IF_DATA XCP /end IF_DATA \
                   /end GROUP";
        let out = roundtrip::<Group>(src);
        assert_eq!(
            out,
            "/begin GROUP G1\n  \"grp desc\"\n  ROOT\n  \
             /begin REF_MEASUREMENT\n    M1 M2 M3\n    M4\n  /end REF_MEASUREMENT\n  \
             /begin REF_CHARACTERISTIC\n    C1\n  /end REF_CHARACTERISTIC\n  \
             /begin FUNCTION_LIST\n    F1 F2\n  /end FUNCTION_LIST\n  \
             /begin SUB_GROUP\n    G2\n  /end SUB_GROUP\n  \
             /begin ANNOTATION\n    ANNOTATION_ORIGIN \"org\"\n  /end ANNOTATION\n  \
             /begin IF_DATA\n    XCP\n  /end IF_DATA\n/end GROUP\n"
        );
        let toks = tokenize(&out).unwrap();
        let root = build_block_tree(&toks).unwrap();
        let g2 = Group::parse(root.child("GROUP").unwrap()).unwrap();
        assert!(g2.root);
        assert_eq!(g2.children.len(), 6);
    }

    #[test]
    fn group_without_root() {
        let out = roundtrip::<Group>("/begin GROUP G \"d\" /end GROUP");
        assert_eq!(out, "/begin GROUP G\n  \"d\"\n/end GROUP\n");
    }

    #[test]
    fn frame_full() {
        let src = "/begin FRAME Fr1 \"frame desc\" 3 100 FRAME_MEASUREMENT M1 M2 M3 \
                   /begin IF_DATA XCP_STUFF /end IF_DATA /end FRAME";
        let out = roundtrip::<Frame>(src);
        assert_eq!(
            out,
            "/begin FRAME Fr1\n  \"frame desc\"\n  3\n  100\n  \
             FRAME_MEASUREMENT M1\n  M2\n  M3\n  \
             /begin IF_DATA\n    XCP_STUFF\n  /end IF_DATA\n/end FRAME\n"
        );
    }

    #[test]
    fn frame_without_measurements() {
        let out = roundtrip::<Frame>("/begin FRAME Fr1 \"d\" 6 10 /end FRAME");
        assert_eq!(out, "/begin FRAME Fr1\n  \"d\"\n  6\n  10\n/end FRAME\n");
    }

    #[test]
    fn frame_hex_params_write_decimal() {
        let out = roundtrip::<Frame>("/begin FRAME F \"d\" 0x3 0x64 /end FRAME");
        assert_eq!(out, "/begin FRAME F\n  \"d\"\n  3\n  100\n/end FRAME\n");
    }

    #[test]
    fn frame_measurement_tag_only_empty_list() {
        let out = roundtrip::<Frame>("/begin FRAME F \"d\" 1 2 FRAME_MEASUREMENT /end FRAME");
        assert_eq!(out, "/begin FRAME F\n  \"d\"\n  1\n  2\n/end FRAME\n");
    }

    #[test]
    fn ref_group_folds_by_columns() {
        let out = roundtrip::<RefGroup>("/begin REF_GROUP A B C D /end REF_GROUP");
        assert_eq!(out, "/begin REF_GROUP\n  A B C\n  D\n/end REF_GROUP\n");
    }

    #[test]
    fn ref_block_empty_tolerated() {
        let out = roundtrip::<RefMeasurement>("/begin REF_MEASUREMENT /end REF_MEASUREMENT");
        assert_eq!(out, "/begin REF_MEASUREMENT\n/end REF_MEASUREMENT\n");
    }

    #[test]
    fn dependent_characteristic_roundtrip() {
        let out = roundtrip::<DependentCharacteristic>(
            "/begin DEPENDENT_CHARACTERISTIC \"XCP_DEFAULT_1\" C1 C2 C3 C4 /end DEPENDENT_CHARACTERISTIC",
        );
        assert_eq!(
            out,
            "/begin DEPENDENT_CHARACTERISTIC\n  \"XCP_DEFAULT_1\"\n  C1 C2 C3\n  \
             C4\n/end DEPENDENT_CHARACTERISTIC\n"
        );
    }

    #[test]
    fn virtual_characteristic_roundtrip() {
        let out = roundtrip::<VirtualCharacteristic>(
            "/begin VIRTUAL_CHARACTERISTIC \"f(x)=x*2\" V1 V2 /end VIRTUAL_CHARACTERISTIC",
        );
        assert_eq!(
            out,
            "/begin VIRTUAL_CHARACTERISTIC\n  \"f(x)=x*2\"\n  V1 V2\n/end VIRTUAL_CHARACTERISTIC\n"
        );
    }

    #[test]
    fn sub_nodes_via_function_children() {
        let out = roundtrip::<SubFunction>("/begin SUB_FUNCTION F1 F2 F3 F4 F5 /end SUB_FUNCTION");
        assert_eq!(
            out,
            "/begin SUB_FUNCTION\n  F1 F2 F3\n  F4 F5\n/end SUB_FUNCTION\n"
        );
        let out = roundtrip::<Virtual>("/begin VIRTUAL M1 M2 /end VIRTUAL");
        assert_eq!(out, "/begin VIRTUAL\n  M1 M2\n/end VIRTUAL\n");
    }
}
