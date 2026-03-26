//! ODX flash/mem types: the ODX-FLASH data model and the PDX package index.
//! This family covers the PDX catalog index (`PdxIndex`/`PdxABlock`/`PdxFile`/
//! `UpdType`), the memory description types (`Mem`, `PhysMem`, `PhysSegment*`/
//! `Segment`, `Flash`, `FlashDataExtern`/`FlashDataIntern`, `EcuMem`/
//! `EcuMemConnector`, `DataBlock`/`DataFile`/`DataFormat`, `Security`,
//! `Session`/`SessionDesc`, `Filter`, `TerminationType`/`RowFragment`/
//! `ValidType`/`PinType`, `VehicleConnector`, and more).
//! ## Facade
//! These types are deeply coupled with `OdxRoot`/`OdxIndex` (`Flash` hangs
//! off the document root and references are resolved through the `IdRef`
//! index), so they are implemented in [`crate::odx`] — including
//! `PdxIndex::get_files`, the hex-data accessors of `FlashDataIntern`, and
//! the `xsi:type` polymorphism of `PhysSegment`/`FlashData`. This module acts
//! as the facade for the flash/mem family: it re-exports those types so that
//! paths like `crate::odx_flash::Mem` are available. The semantics of each
//! type are documented on the type itself in `odx.rs`.

pub use crate::odx::{
    DataBlock, DataBlockType, DataFile, DataFormat, DataFormatType, DatablockRefs, Datablocks,
    EcuMem, EcuMemConnector, EcuMemConnectors, EcuMems, ExpectedIdents, Filter, Filters, Flash,
    FlashClassRefs, FlashClasss, FlashData, FlashDataExtern, FlashDataIntern, FlashDatas, Ident,
    IdentDesc, IdentDescs, LayerRefs, Mem, OwnIdents, PdxABlock, PdxABlocks, PdxFile, PdxFiles,
    PdxIndex, PhysMem, PhysSegment, PhysSegmentAddr, PhysSegmentSize, PhysSegments, PinType,
    RowFragment, Security, Securitys, Segment, Segments, Session, SessionDesc, SessionDescs,
    Sessions, TerminationType, TypeValueElement, UpdType, ValidType, VehicleConnector,
    VehicleConnectorPin, VehicleConnectorPins,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::odx::{BaseDataType, IdRef, NamedDescIdData, SnRef};
    use std::path::Path;

    /// Builds a PDX index containing two categories of ABLOCK.
    fn sample_pdx() -> PdxIndex {
        PdxIndex {
            short_name: Some("CATALOG_1".to_owned()),
            ablocks: Some(PdxABlocks {
                items: vec![
                    PdxABlock {
                        upd: UpdType::New,
                        short_name: Some("BLOCK_A".to_owned()),
                        category: "ODX-DATA".to_owned(),
                        files: Some(PdxFiles {
                            items: vec![
                                PdxFile {
                                    mime_type: Some("application/xml".to_owned()),
                                    creation_date: Some("2024-01-01".to_owned()),
                                    text: Some("a.odx".to_owned()),
                                },
                                PdxFile {
                                    mime_type: None,
                                    creation_date: None,
                                    text: Some("sub/b.odx".to_owned()),
                                },
                            ],
                        }),
                    },
                    PdxABlock {
                        upd: UpdType::Deleted,
                        short_name: None,
                        category: "DOC".to_owned(),
                        files: Some(PdxFiles { items: vec![] }),
                    },
                ],
            }),
        }
    }

    #[test]
    fn pdx_defaults() {
        // The ABLOCK.CATEGORY field defaults to "ODX-DATA"
        assert_eq!(PdxABlock::default().category, "ODX-DATA");
        assert_eq!(UpdType::default(), UpdType::New);
    }

    #[test]
    fn pdx_get_files_filters_category() {
        let pdx = sample_pdx();
        // Empty prefix: existence is not checked, only the category filter applies
        let files = pdx.get_files("ODX-DATA", Path::new(""));
        assert_eq!(files.len(), 2);
        assert!(files[0].ends_with("a.odx"));
        assert!(files[1].ends_with("b.odx"));
        // An ABLOCK with an empty FILES list is skipped (an intentional quirk)
        assert!(pdx.get_files("DOC", Path::new("")).is_empty());
        assert!(pdx.get_files("NOPE", Path::new("")).is_empty());
    }

    #[test]
    fn pdx_get_files_checks_existence() {
        let dir = std::env::temp_dir().join(format!("autors_pdx_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.odx"), b"x").unwrap();
        let pdx = sample_pdx();
        // Non-empty prefix: only files that exist on disk are returned
        let files = pdx.get_files("ODX-DATA", &dir);
        assert_eq!(files.len(), 1);
        assert!(files[0].ends_with("a.odx"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn pdx_index_xml_roundtrip() {
        let xml = r#"<CATALOG><SHORT-NAME>C</SHORT-NAME><ABLOCKS><ABLOCK UPD="CHANGED"><SHORT-NAME>B</SHORT-NAME><CATEGORY>ODX-DATA</CATEGORY><FILES><FILE MIME-TYPE="application/xml" CREATION-DATE="2024-01-01">a.odx</FILE></FILES></ABLOCK></ABLOCKS></CATALOG>"#;
        let pdx: PdxIndex = quick_xml::de::from_str(xml).unwrap();
        assert_eq!(pdx.short_name.as_deref(), Some("C"));
        let ab = &pdx.ablocks.as_ref().unwrap().items[0];
        assert_eq!(ab.upd, UpdType::Changed);
        assert_eq!(
            ab.files.as_ref().unwrap().items[0].text.as_deref(),
            Some("a.odx")
        );
        // Write out and parse back: semantically equivalent
        let out = quick_xml::se::to_string(&pdx).unwrap();
        let pdx2: PdxIndex = quick_xml::de::from_str(&out).unwrap();
        assert_eq!(pdx, pdx2);
    }

    #[test]
    fn flash_data_intern_bytes() {
        // The hex-data accessors format bytes as `{0:X2}` pairs and parse two characters per byte
        let mut fd = FlashDataIntern::default();
        fd.set_data_bytes(&[0x12, 0xAB, 0x00]);
        assert_eq!(fd.data.as_deref(), Some("12AB00"));
        assert_eq!(fd.data_bytes(), vec![0x12, 0xAB, 0x00]);
        fd.set_data_bytes(&[]);
        assert_eq!(fd.data, None);
        assert!(fd.data_bytes().is_empty());
    }

    #[test]
    fn datablock_type_accessor() {
        // The `Type` accessor: an unparseable value yields UNKNOWN
        let mut db = DataBlock::default();
        assert_eq!(db.block_type(), DataBlockType::Unknown);
        db.set_block_type(DataBlockType::Code);
        assert_eq!(db.type_.as_deref(), Some("CODE"));
        assert_eq!(db.block_type(), DataBlockType::Code);
        db.type_ = Some("junk".to_owned());
        assert_eq!(db.block_type(), DataBlockType::Unknown);
    }

    #[test]
    fn mem_xml_roundtrip() {
        let intern = FlashData::Intern(FlashDataIntern {
            id: Some("FD.1".to_owned()),
            short_name: Some("FD_INTERN".to_owned()),
            dataformat: DataFormat {
                selection: DataFormatType::Binary,
            },
            data: Some("00FF".to_owned()),
            ..Default::default()
        });
        let extern_ = FlashData::Extern(FlashDataExtern {
            id: Some("FD.2".to_owned()),
            short_name: Some("FD_EXTERN".to_owned()),
            dataformat: DataFormat {
                selection: DataFormatType::IntelHex,
            },
            datafile: DataFile {
                latebound_datafile: true,
                text: Some("app.hex".to_owned()),
            },
            ..Default::default()
        });
        let mem = Mem {
            sessions: Some(Sessions {
                items: vec![Session {
                    id: Some("S.1".to_owned()),
                    short_name: Some("SESSION_1".to_owned()),
                    datablock_refs: Some(DatablockRefs {
                        items: vec![IdRef::new("DB.1")],
                    }),
                    expected_idents: Some(ExpectedIdents {
                        items: vec![Ident {
                            id: None,
                            short_name: None,
                            long_name: None,
                            sdgs: None,
                            ident_value: TypeValueElement {
                                type_: BaseDataType::AUint32,
                                value: Some("0x22".to_owned()),
                            },
                        }],
                    }),
                    securitys: None,
                    ..Default::default()
                }],
            }),
            datablocks: Some(Datablocks {
                items: vec![DataBlock {
                    id: Some("DB.1".to_owned()),
                    short_name: Some("DATABLOCK_1".to_owned()),
                    type_: Some("CODE".to_owned()),
                    flashdata_ref: Some(IdRef::new("FD.1")),
                    segments: Some(Segments {
                        items: vec![Segment {
                            id: None,
                            short_name: None,
                            long_name: None,
                            sdgs: None,
                            source_start_address: Some("0x8000000".to_owned()),
                            source_end_address: Some("0x800FFFF".to_owned()),
                            uncompressed_size: 65536,
                        }],
                    }),
                    own_idents: None,
                    securitys: Some(Securitys {
                        items: vec![Security {
                            security_method: TypeValueElement {
                                type_: BaseDataType::AUint32,
                                value: Some("0x01".to_owned()),
                            },
                            ..Default::default()
                        }],
                    }),
                    filters: Some(Filters {
                        items: vec![Filter {
                            filter_start: Some("0x0".to_owned()),
                            filter_end: Some("0xF".to_owned()),
                        }],
                    }),
                    ..Default::default()
                }],
            }),
            flashdatas: Some(FlashDatas {
                items: vec![intern, extern_],
            }),
        };
        let out = quick_xml::se::to_string(&mem).unwrap();
        // xsi:type polymorphism of FLASHDATA
        assert!(out.contains(r#"xsi:type="INTERN-FLASHDATA""#), "{out}");
        assert!(out.contains(r#"xsi:type="EXTERN-FLASHDATA""#), "{out}");
        assert!(out.contains("<DATABLOCK"), "{out}");
        let mem2: Mem = quick_xml::de::from_str(&out).unwrap();
        assert_eq!(mem, mem2);
    }

    #[test]
    fn phys_segment_polymorphic_xml() {
        let xml = r#"<PHYS-SEGMENT xsi:type="ADDRDEF-PHYS-SEGMENT" ID="PS.1"><SHORT-NAME>SEG</SHORT-NAME><FILLBYTE>0xFF</FILLBYTE><START-ADDRESS>0x0</START-ADDRESS><END-ADDRESS>0xFF</END-ADDRESS></PHYS-SEGMENT>"#;
        let seg: PhysSegment = quick_xml::de::from_str(xml).unwrap();
        match &seg {
            PhysSegment::Addr(a) => {
                assert_eq!(a.end_address.as_deref(), Some("0xFF"));
                assert_eq!(a.fillbyte.as_deref(), Some("0xFF"));
            }
            other => panic!("unexpected {other:?}"),
        }
        let out = quick_xml::se::to_string(&seg).unwrap();
        assert!(out.contains(r#"xsi:type="ADDRDEF-PHYS-SEGMENT""#), "{out}");
        let seg2: PhysSegment = quick_xml::de::from_str(&out).unwrap();
        assert_eq!(seg, seg2);
        // A missing xsi:type is an error (the abstract base cannot be instantiated)
        assert!(quick_xml::de::from_str::<PhysSegment>(r#"<PHYS-SEGMENT ID="X" />"#).is_err());
    }

    #[test]
    fn flash_struct_construction() {
        let flash = Flash {
            ecu_mems: Some(EcuMems {
                items: vec![EcuMem {
                    id: Some("EM.1".to_owned()),
                    mem: Some(Mem::default()),
                    phys_mem: Some(PhysMem {
                        id: None,
                        short_name: None,
                        long_name: None,
                        sdgs: None,
                        phys_segments: Some(PhysSegments {
                            items: vec![
                                PhysSegment::Addr(PhysSegmentAddr {
                                    id: Some("PS.1".to_owned()),
                                    start_address: Some("0x0".to_owned()),
                                    end_address: Some("0xFFFF".to_owned()),
                                    ..Default::default()
                                }),
                                PhysSegment::Size(PhysSegmentSize {
                                    id: Some("PS.2".to_owned()),
                                    start_address: Some("0x10000".to_owned()),
                                    size: Some("0x8000".to_owned()),
                                    ..Default::default()
                                }),
                            ],
                        }),
                    }),
                    ..Default::default()
                }],
            }),
            ecu_mem_connectors: Some(EcuMemConnectors {
                items: vec![EcuMemConnector {
                    id: Some("EMC.1".to_owned()),
                    ecu_mem_ref: Some(IdRef::new("EM.1")),
                    layer_refs: Some(LayerRefs {
                        items: vec![IdRef::new("L.1")],
                    }),
                    flash_classs: Some(FlashClasss {
                        items: vec![NamedDescIdData {
                            short_name: Some("FC".to_owned()),
                            ..Default::default()
                        }],
                    }),
                    session_descs: Some(SessionDescs {
                        items: vec![SessionDesc {
                            direction: Some("DOWNLOAD".to_owned()),
                            session_snref: Some(SnRef {
                                short_name: Some("SESSION_1".to_owned()),
                            }),
                            partnumber: Some("12345".to_owned()),
                            priority: 1,
                            ..Default::default()
                        }],
                    }),
                    ident_descs: Some(IdentDescs {
                        items: vec![IdentDesc {
                            diag_comm_snref: Some(SnRef {
                                short_name: Some("RV".to_owned()),
                            }),
                            ..Default::default()
                        }],
                    }),
                    ..Default::default()
                }],
            }),
            ..Default::default()
        };
        let em = &flash.ecu_mems.as_ref().unwrap().items[0];
        assert_eq!(
            em.phys_mem
                .as_ref()
                .unwrap()
                .phys_segments
                .as_ref()
                .unwrap()
                .items
                .len(),
            2
        );
        let emc = &flash.ecu_mem_connectors.as_ref().unwrap().items[0];
        let sd = &emc.session_descs.as_ref().unwrap().items[0];
        // display_name(): LONG-NAME is preferred, falling back to SHORT-NAME
        assert_eq!(sd.display_name(), "");
        assert_eq!(sd.priority, 1);
        assert_eq!(
            emc.layer_refs.as_ref().unwrap().items[0].id_ref.as_deref(),
            Some("L.1")
        );
    }

    #[test]
    fn vehicle_connector_construction() {
        let vc = VehicleConnector {
            short_name: Some("OBD".to_owned()),
            long_name: Some("OBD-II connector".to_owned()),
            vehicle_connector_pins: Some(VehicleConnectorPins {
                items: vec![VehicleConnectorPin {
                    id: Some("PIN.6".to_owned()),
                    type_: PinType::Hi,
                    pin_number: 6,
                    ..Default::default()
                }],
            }),
        };
        assert_eq!(vc.display_name(), "OBD-II connector");
        assert_eq!(
            vc.vehicle_connector_pins.as_ref().unwrap().items[0].type_,
            PinType::Hi
        );
    }
}
