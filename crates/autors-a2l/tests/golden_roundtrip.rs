//! Golden round-trip tests. `tests/data/golden.a2l` is a checked-in realistic
//! A2L file in canonical written form. The tests verify that autors-a2l parse →
//! write → re-parse is semantically equivalent.

use autors_a2l::model::module::ModuleChild;
use autors_a2l::model::project::ProjectChild;
use autors_a2l::Project;

const GOLDEN: &str = include_str!("data/golden.a2l");

fn find_module<'a>(p: &'a Project, name: &str) -> &'a autors_a2l::Module {
    p.modules().find(|m| m.name == name).expect("module MOD1")
}

#[test]
fn parses_golden_file() {
    let p = Project::parse_str(GOLDEN).expect("parse golden.a2l");
    assert_eq!(p.named.name, "GoldenProj");
    assert!(p.header().is_some());
    assert_eq!(p.modules().count(), 1);

    let m = find_module(&p, "MOD1");
    // Spot-check that key child blocks parse into typed variants.
    let has = |pred: fn(&ModuleChild) -> bool| m.children.iter().any(pred);
    assert!(has(|c| matches!(c, ModuleChild::ModCommon(_))));
    assert!(has(|c| matches!(c, ModuleChild::RecordLayout(_))));
    assert!(has(|c| matches!(c, ModuleChild::CompuMethod(_))));
    assert!(has(|c| matches!(c, ModuleChild::CompuTab(_))));
    assert!(has(|c| matches!(c, ModuleChild::CompuVtab(_))));
    assert!(has(|c| matches!(c, ModuleChild::Characteristic(_))));
    assert!(has(|c| matches!(c, ModuleChild::AxisPts(_))));
    assert!(has(|c| matches!(c, ModuleChild::Unit(_))));
    assert!(has(|c| matches!(c, ModuleChild::Function(_))));
    assert!(has(|c| matches!(c, ModuleChild::Group(_))));
    assert!(has(|c| matches!(c, ModuleChild::VariantCoding(_))));

    let meas = m
        .children
        .iter()
        .find_map(|c| match c {
            ModuleChild::Measurement(mm) if mm.named.name == "EngineSpeed" => Some(mm),
            _ => None,
        })
        .expect("measurement EngineSpeed");
    assert_eq!(meas.addr.address, Some(0x1000));
    assert_eq!(meas.conv.conversion, "CONV_LIN");

    let ch = m
        .children
        .iter()
        .find_map(|c| match c {
            ModuleChild::Characteristic(cc) if cc.named.name == "KfMap" => Some(cc),
            _ => None,
        })
        .expect("characteristic KfMap");
    assert_eq!(ch.addr.address, Some(0x803DAC));
    assert!(ch.children.iter().any(|c| matches!(
        c,
        autors_a2l::model::characteristic::CharacteristicChild::AxisDescr(_)
    )));
}

#[test]
fn golden_roundtrip_semantic_equality() {
    let p1 = Project::parse_str(GOLDEN).expect("parse golden.a2l");
    let out1 = p1.write_string().expect("write #1");
    let p2 = Project::parse_str(&out1).expect("re-parse #1");
    assert_eq!(
        p1, p2,
        "parse(golden) and parse(write(parse(golden))) must be semantically equal"
    );

    let out2 = p2.write_string().expect("write #2");
    assert_eq!(
        out1, out2,
        "two consecutive writes must reach the same fixed point"
    );
}

#[test]
fn golden_header_version_preserved() {
    let p = Project::parse_str(GOLDEN).expect("parse");
    let header = p.header().expect("HEADER");
    let out = p.write_string().unwrap();
    let p2 = Project::parse_str(&out).unwrap();
    assert_eq!(p2.header(), Some(header));
    assert_eq!(
        p.children
            .iter()
            .filter(|c| matches!(c, ProjectChild::Header(_)))
            .count(),
        1
    );
}
