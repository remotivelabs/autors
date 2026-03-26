//! PDX containers (zip packages) and joint loading of cross-file `DOCREF`
//! references.
//! An ODX data set is usually split across several XML documents that
//! reference each other via `ID-REF`/`DOCREF` attribute pairs. This module
//! loads such a set as one unit:
//! - [`OdxSource`] abstracts the document source: [`DirSource`] scans a
//!   filesystem directory, [`PdxSource`] reads members of a PDX zip
//!   container via [`PdxPackage`].
//! - Candidate documents are enumerated by locating `index.xml` (a
//!   `PdxIndex` catalog, category `ODX-DATA`) first, falling back to a
//!   `*.xml` scan when the index is missing or yields no usable entries.
//!   `index.xml` itself and files that do not look like XML are filtered
//!   out.
//! - The main document is either matched by file name
//!   ([`OdxDocumentSet::open_path`]) or auto-detected by content tag —
//!   `DIAG-LAYER-CONTAINER`, `VEHICLE-INFO-SPEC`, `FLASH`, in that priority
//!   order ([`OdxDocumentSet::open_dir`], [`OdxDocumentSet::open_pdx`]).
//! - Remaining candidates stay in the "available files" list; `DOCREF`
//!   references trigger on-demand lazy loading of the referenced document
//!   into the "external references" list ([`OdxDocumentSet::resolve`]).
//! ## Notes on behavior
//! - [`PdxPackage`] applies the same `index.xml` → `ODX-DATA` member
//!   enumeration → glob fallback logic directly inside the zip container,
//!   so a packed `.pdx` behaves like an extracted directory.
//! - "Looks like an XML file" means the first non-empty trimmed line starts
//!   with `<?xml` or `<ODX` (both are accepted: files written by this
//!   library carry an `<?xml` declaration, while accepting only `<ODX`
//!   would reject declaration-less input).
//! - Unresolvable references cannot be flagged on the already-parsed tree;
//!   instead [`OdxDocumentSet`] records a failure set keyed by
//!   `(DOCREF, ID-REF)`, queryable via [`OdxDocumentSet::failed_to_load`].
//! - Parse/load warnings are collected as plain strings in
//!   [`OdxDocumentSet::parser_events`].
//! - Fallback directory/member scans are sorted by file name to keep
//!   enumeration deterministic; the `index.xml` path keeps its `FILE`
//!   entry order.

use std::collections::HashSet;
use std::io::{BufReader, Read, Seek};
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::odx::{IdObject, IdRef, OdxFile, PdxIndex};

/// PDX category used for candidate enumeration (matches the `PdxABlock`
/// default value `"ODX-DATA"`).
const PDX_CATEGORY_ODX_DATA: &str = "ODX-DATA";

/// File name of the PDX index catalog.
const INDEX_XML: &str = "index.xml";

/// Content tags used to auto-locate the main document in directory mode
/// (top-level `OdxRoot` elements, priority from left to right).
const ROOT_DETECT_TAGS: [&str; 3] = ["DIAG-LAYER-CONTAINER", "VEHICLE-INFO-SPEC", "FLASH"];

// ============================================================================
// File name helpers (also applicable to zip member names)
// ============================================================================

/// Returns the last segment of a path/member name (accepts both `/` and `\`
/// separators).
fn base_name(name: &str) -> &str {
    name.rsplit(['/', '\\']).next().unwrap_or(name)
}

/// Strips the directory part and the last extension.
fn file_stem(name: &str) -> &str {
    let base = base_name(name);
    match base.rfind('.') {
        Some(i) if i > 0 => &base[..i],
        _ => base,
    }
}

/// Lowercased full file name, for name-based comparison.
fn file_name_lower(name: &str) -> String {
    base_name(name).to_lowercase()
}

/// Case-insensitive string equality.
fn eq_icase(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

/// Whether the text "looks like an XML file": the first non-empty trimmed
/// line starts with an XML prefix. Both `<?xml` and `<ODX` are accepted
/// (see module-level notes).
fn looks_like_xml(text: &str) -> bool {
    for line in text.lines() {
        if line.is_empty() {
            continue;
        }
        let t = line.trim_start();
        return t.starts_with("<?xml") || t.starts_with("<ODX");
    }
    false
}

/// Content containment check: the first non-empty line must pass the prefix
/// check and **does not** take part in content matching (an intentional
/// `if/else if` quirk, kept as-is); any later line containing `needle` is
/// a hit.
fn content_contains(text: &str, needle: &str) -> bool {
    let mut first_checked = false;
    for line in text.lines() {
        if !first_checked && !line.is_empty() {
            if !looks_like_xml(line) {
                return false;
            }
            first_checked = true;
        } else if line.contains(needle) {
            return true;
        }
    }
    false
}

// ============================================================================
// ============================================================================

/// Provides named ODX XML documents to the multi-file loader.
pub trait OdxSource {
    fn candidates(&mut self) -> Result<Vec<String>>;
    fn read_xml(&mut self, name: &str) -> Result<String>;
}

pub struct DirSource {
    dir: PathBuf,
}

impl DirSource {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        DirSource { dir: dir.into() }
    }
}

impl OdxSource for DirSource {
    fn candidates(&mut self) -> Result<Vec<String>> {
        let index_path = self.dir.join(INDEX_XML);
        let mut list: Vec<PathBuf> = Vec::new();
        if index_path.is_file() {
            if let Ok(text) = std::fs::read_to_string(&index_path) {
                if let Ok(index) = quick_xml::de::from_str::<PdxIndex>(&text) {
                    list = index.get_files(PDX_CATEGORY_ODX_DATA, &self.dir);
                }
            }
        }
        if list.is_empty() {
            if let Ok(rd) = std::fs::read_dir(&self.dir) {
                let mut names: Vec<PathBuf> = rd
                    .filter_map(std::result::Result::ok)
                    .map(|e| e.path())
                    .filter(|p| {
                        p.is_file()
                            && p.extension()
                                .is_some_and(|e| e.to_string_lossy().eq_ignore_ascii_case("xml"))
                    })
                    .collect();
                names.sort();
                list = names;
            }
        }
        let mut out = Vec::new();
        for p in list {
            if file_name_lower(&p.to_string_lossy()) == INDEX_XML {
                continue;
            }
            match std::fs::read_to_string(&p) {
                Ok(text) if looks_like_xml(&text) => out.push(p.to_string_lossy().into_owned()),
                _ => continue,
            }
        }
        Ok(out)
    }

    fn read_xml(&mut self, name: &str) -> Result<String> {
        Ok(std::fs::read_to_string(name)?)
    }
}

// ============================================================================
// ============================================================================

/// Provides read access to the members of a PDX zip archive.
pub struct PdxPackage<R> {
    archive: zip::ZipArchive<R>,
}

impl PdxPackage<BufReader<std::fs::File>> {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let f = std::fs::File::open(path.as_ref())?;
        Self::from_reader(BufReader::new(f))
    }
}

impl<R: Read + Seek> PdxPackage<R> {
    pub fn from_reader(reader: R) -> Result<Self> {
        Ok(PdxPackage {
            archive: zip::ZipArchive::new(reader)?,
        })
    }

    pub fn len(&self) -> usize {
        self.archive.len()
    }

    pub fn is_empty(&self) -> bool {
        self.archive.len() == 0
    }

    pub fn member_names(&mut self) -> Vec<String> {
        let mut out = Vec::new();
        for i in 0..self.archive.len() {
            if let Ok(f) = self.archive.by_index(i) {
                if !f.is_dir() {
                    out.push(f.name().to_owned());
                }
            }
        }
        out
    }

    pub fn read_member(&mut self, name: &str) -> Result<String> {
        let mut f = self.archive.by_name(name)?;
        let mut s = String::new();
        f.read_to_string(&mut s)?;
        Ok(s)
    }

    pub fn index_xml(&mut self) -> Result<Option<PdxIndex>> {
        let name = self
            .member_names()
            .into_iter()
            .find(|n| eq_icase(base_name(n), INDEX_XML));
        let Some(name) = name else { return Ok(None) };
        let text = self.read_member(&name)?;
        let index =
            quick_xml::de::from_str::<PdxIndex>(&text).map_err(|e| Error::Xml(e.to_string()))?;
        Ok(Some(index))
    }

    pub fn odx_member_names(&mut self) -> Result<Vec<String>> {
        let mut list: Vec<String> = Vec::new();
        if let Some(index) = self.index_xml()? {
            list = self.members_of_category(&index, PDX_CATEGORY_ODX_DATA);
        }
        if list.is_empty() {
            let mut names: Vec<String> = self
                .member_names()
                .into_iter()
                .filter(|n| {
                    !eq_icase(base_name(n), INDEX_XML) && n.to_lowercase().ends_with(".xml")
                })
                .collect();
            names.sort();
            list = names;
        }
        let mut out = Vec::new();
        for n in list {
            if eq_icase(base_name(&n), INDEX_XML) {
                continue;
            }
            match self.read_member(&n) {
                Ok(text) if looks_like_xml(&text) => out.push(n),
                _ => continue,
            }
        }
        Ok(out)
    }

    fn members_of_category(&mut self, index: &PdxIndex, category: &str) -> Vec<String> {
        let all = self.member_names();
        let mut out = Vec::new();
        let Some(ablocks) = &index.ablocks else {
            return out;
        };
        for ablock in &ablocks.items {
            if category != ablock.category {
                continue;
            }
            let Some(files) = &ablock.files else {
                continue;
            };
            if files.items.is_empty() {
                continue;
            }
            for f in &files.items {
                let Some(text) = &f.text else { continue };
                let norm = text.replace('\\', "/");
                if let Some(m) = all.iter().find(|m| {
                    eq_icase(m.as_str(), norm.as_str())
                        || m.to_lowercase()
                            .ends_with(&format!("/{}", norm.to_lowercase()))
                }) {
                    out.push(m.clone());
                }
            }
        }
        out
    }
}

pub struct PdxSource<R> {
    package: PdxPackage<R>,
}

impl PdxSource<BufReader<std::fs::File>> {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Ok(PdxSource {
            package: PdxPackage::open(path)?,
        })
    }
}

impl<R: Read + Seek> PdxSource<R> {
    pub fn from_reader(reader: R) -> Result<Self> {
        Ok(PdxSource {
            package: PdxPackage::from_reader(reader)?,
        })
    }

    pub fn package(&mut self) -> &mut PdxPackage<R> {
        &mut self.package
    }
}

impl<R: Read + Seek> OdxSource for PdxSource<R> {
    fn candidates(&mut self) -> Result<Vec<String>> {
        self.package.odx_member_names()
    }

    fn read_xml(&mut self, name: &str) -> Result<String> {
        self.package.read_member(name)
    }
}

// ============================================================================
// ============================================================================

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct FailedRef {
    docref: Option<String>,
    id_ref: String,
}

/// 73353, 73482, 74147).
pub struct OdxDocumentSet {
    main: OdxFile,
    externals: Vec<OdxFile>,
    available: Vec<String>,
    source: Box<dyn OdxSource>,
    failed: HashSet<FailedRef>,
    parser_events: Vec<String>,
}

impl OdxDocumentSet {
    /// `AvailableFiles`.
    pub fn open_path(path: impl AsRef<Path>) -> Result<OdxDocumentSet> {
        let path = path.as_ref();
        let dir = match path.parent() {
            Some(d) if !d.as_os_str().is_empty() => d.to_path_buf(),
            _ => PathBuf::from("."),
        };
        let target = file_name_lower(&path.to_string_lossy());
        let mut source = DirSource::new(dir);
        let candidates = source.candidates()?;
        let mut main: Option<OdxFile> = None;
        let mut available = Vec::new();
        for c in candidates {
            if main.is_none() && file_name_lower(&c) == target {
                let text = source.read_xml(&c)?;
                let mut f = OdxFile::parse_str(&text)?;
                f.source_file = Some(c);
                main = Some(f);
            } else {
                available.push(c);
            }
        }
        let main = match main {
            Some(m) => m,
            None => {
                let mut f = OdxFile::open(path)?;
                available.retain(|c| file_name_lower(c) != target);
                f.source_file = Some(path.to_string_lossy().into_owned());
                f
            }
        };
        Ok(OdxDocumentSet {
            main,
            externals: Vec::new(),
            available,
            source: Box::new(source),
            failed: HashSet::new(),
            parser_events: Vec::new(),
        })
    }

    pub fn open_dir(dir: impl AsRef<Path>) -> Result<OdxDocumentSet> {
        let source = DirSource::new(dir.as_ref().to_path_buf());
        Self::open_autodetect(Box::new(source))
    }

    pub fn open_pdx(path: impl AsRef<Path>) -> Result<OdxDocumentSet> {
        let source = PdxSource::open(path)?;
        Self::open_autodetect(Box::new(source))
    }

    fn open_autodetect(mut source: Box<dyn OdxSource>) -> Result<OdxDocumentSet> {
        let candidates = source.candidates()?;
        let mut available = candidates;
        let mut main: Option<(usize, OdxFile)> = None;
        'outer: for tag in ROOT_DETECT_TAGS {
            for (i, name) in available.iter().enumerate() {
                let text = source.read_xml(name)?;
                if content_contains(&text, tag) {
                    let mut f = OdxFile::parse_str(&text)?;
                    f.source_file = Some(name.clone());
                    main = Some((i, f));
                    break 'outer;
                }
            }
        }
        let Some((idx, main)) = main else {
            return Err(Error::Parse(
                "no ODX root document found (DIAG-LAYER-CONTAINER/VEHICLE-INFO-SPEC/FLASH)"
                    .to_owned(),
            ));
        };
        available.remove(idx);
        Ok(OdxDocumentSet {
            main,
            externals: Vec::new(),
            available,
            source,
            failed: HashSet::new(),
            parser_events: Vec::new(),
        })
    }

    pub fn main(&self) -> &OdxFile {
        &self.main
    }

    pub fn external_references(&self) -> &[OdxFile] {
        &self.externals
    }

    pub fn available_files(&self) -> &[String] {
        &self.available
    }

    pub fn parser_events(&self) -> &[String] {
        &self.parser_events
    }

    pub fn failed_to_load(&self, id_ref: &IdRef) -> bool {
        let Some(id) = non_empty(id_ref.id_ref.as_deref()) else {
            return false;
        };
        self.failed.contains(&FailedRef {
            docref: non_empty(id_ref.docref.as_deref()).map(str::to_owned),
            id_ref: id.to_owned(),
        })
    }

    pub fn find_id(&self, id: &str) -> Option<IdObject<'_>> {
        self.main
            .odx
            .find_id(id)
            .or_else(|| self.externals.iter().find_map(|f| f.odx.find_id(id)))
    }

    /// Resolves a local or cross-document ID reference, loading its document on demand.
    pub fn resolve(&mut self, id_ref: &IdRef) -> Option<IdObject<'_>> {
        let id = non_empty(id_ref.id_ref.as_deref())?.to_owned();
        let docref = non_empty(id_ref.docref.as_deref()).map(str::to_owned);
        let key = FailedRef {
            docref: docref.clone(),
            id_ref: id.clone(),
        };
        if self.failed.contains(&key) {
            self.warn_failed(&id, docref.as_deref());
            return None;
        }
        match docref {
            None => {
                if self.main.odx.find_id(&id).is_some() {
                    return self.main.odx.find_id(&id);
                }
                self.failed.insert(key);
                self.warn_failed(&id, None);
                None
            }
            Some(doc) => self.resolve_cross(&id, &doc),
        }
    }

    fn resolve_cross(&mut self, id: &str, doc: &str) -> Option<IdObject<'_>> {
        enum Probe {
            FoundAt(usize),
            DocMatchedNoId,
            NotLoaded,
        }
        let probe = {
            let mut probe = Probe::NotLoaded;
            for (i, f) in std::iter::once(&self.main)
                .chain(self.externals.iter())
                .enumerate()
            {
                if f.odx.find_id(id).is_some() {
                    probe = Probe::FoundAt(i);
                    break;
                }
                if f.source_file
                    .as_deref()
                    .is_some_and(|s| eq_icase(file_stem(s), doc))
                {
                    probe = Probe::DocMatchedNoId;
                    break;
                }
            }
            probe
        };
        match probe {
            Probe::FoundAt(0) => self.main.odx.find_id(id),
            Probe::FoundAt(i) => self.externals[i - 1].odx.find_id(id),
            Probe::DocMatchedNoId => {
                self.mark_failed(id, Some(doc));
                None
            }
            Probe::NotLoaded => match self.find_available(id, doc) {
                Some(name) => match self.load_external(&name) {
                    Ok(()) => self.resolve_cross(id, doc),
                    Err(e) => {
                        self.parser_events
                            .push(format!("failed to load referenced document '{name}': {e}"));
                        self.mark_failed(id, Some(doc));
                        None
                    }
                },
                None => {
                    self.mark_failed(id, Some(doc));
                    None
                }
            },
        }
    }

    fn find_available(&mut self, id: &str, doc: &str) -> Option<String> {
        if let Some(n) = self.available.iter().find(|n| eq_icase(file_stem(n), doc)) {
            return Some(n.clone());
        }
        let needle = format!("ID-REF=\"{id}\"");
        for i in 0..self.available.len() {
            let Ok(text) = self.source.read_xml(&self.available[i].clone()) else {
                continue;
            };
            if content_contains(&text, &needle) {
                return Some(self.available[i].clone());
            }
        }
        None
    }

    fn load_external(&mut self, name: &str) -> Result<()> {
        let text = self.source.read_xml(name)?;
        let mut f = OdxFile::parse_str(&text)?;
        f.source_file = Some(name.to_owned());
        self.available.retain(|n| n != name);
        self.externals.push(f);
        Ok(())
    }

    fn mark_failed(&mut self, id: &str, doc: Option<&str>) {
        self.failed.insert(FailedRef {
            docref: doc.map(str::to_owned),
            id_ref: id.to_owned(),
        });
        self.warn_failed(id, doc);
    }

    fn warn_failed(&mut self, id: &str, doc: Option<&str>) {
        let doc = doc.unwrap_or_default();
        self.parser_events.push(format!(
            "reference '{id}' could not be resolved (document '{doc}')"
        ));
    }
}

fn non_empty(s: Option<&str>) -> Option<&str> {
    s.filter(|v| !v.is_empty())
}

// ============================================================================
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::atomic::{AtomicU32, Ordering};

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> TempDir {
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!(
                "autors_odx_mf_{}_{}_{}",
                std::process::id(),
                tag,
                n
            ));
            std::fs::create_dir_all(&dir).unwrap();
            TempDir(dir)
        }

        fn write(&self, name: &str, content: &str) -> PathBuf {
            let p = self.0.join(name);
            std::fs::write(&p, content).unwrap();
            p
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    const MAIN_XML: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<ODX MODEL-VERSION="2.2.0">
	<DIAG-LAYER-CONTAINER ID="DLC.Main">
		<ECU-SHARED-DATAS>
			<ECU-SHARED-DATA ID="ESD.Main">
				<SHORT-NAME>MainESD</SHORT-NAME>
				<DIAG-DATA-DICTIONARY-SPEC>
					<STRUCTURES>
						<STRUCTURE ID="ST.Main">
							<SHORT-NAME>MainStruct</SHORT-NAME>
							<PARAMS>
								<PARAM xsi:type="VALUE" SEMANTIC="DATA">
									<SHORT-NAME>P1</SHORT-NAME>
									<DOP-REF ID-REF="DOP.Ext" DOCREF="ext" DOCTYPE="SHARED-DATA" />
								</PARAM>
							</PARAMS>
						</STRUCTURE>
					</STRUCTURES>
				</DIAG-DATA-DICTIONARY-SPEC>
			</ECU-SHARED-DATA>
		</ECU-SHARED-DATAS>
	</DIAG-LAYER-CONTAINER>
</ODX>
"#;

    const EXT_XML: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<ODX MODEL-VERSION="2.2.0">
	<DIAG-LAYER-CONTAINER ID="DLC.Ext">
		<ECU-SHARED-DATAS>
			<ECU-SHARED-DATA ID="ESD.Ext">
				<SHORT-NAME>ExtESD</SHORT-NAME>
				<DIAG-DATA-DICTIONARY-SPEC>
					<DATA-OBJECT-PROPS>
						<DATA-OBJECT-PROP ID="DOP.Ext">
							<SHORT-NAME>ExtDop</SHORT-NAME>
						</DATA-OBJECT-PROP>
					</DATA-OBJECT-PROPS>
				</DIAG-DATA-DICTIONARY-SPEC>
			</ECU-SHARED-DATA>
		</ECU-SHARED-DATAS>
	</DIAG-LAYER-CONTAINER>
</ODX>
"#;

    fn docref(id: &str, doc: &str) -> IdRef {
        IdRef {
            id_ref: Some(id.to_owned()),
            docref: Some(doc.to_owned()),
            doctype: None,
        }
    }

    fn make_dir_fixture(tag: &str) -> TempDir {
        let t = TempDir::new(tag);
        t.write("main.xml", MAIN_XML);
        t.write("ext.xml", EXT_XML);
        t
    }

    fn make_pdx(dir: &TempDir, members: &[&str]) -> PathBuf {
        let pdx_path = dir.0.join("pack.pdx");
        let f = std::fs::File::create(&pdx_path).unwrap();
        let mut zw = zip::ZipWriter::new(f);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        for m in members {
            zw.start_file(*m, opts).unwrap();
            let data = std::fs::read(dir.0.join(m)).unwrap();
            zw.write_all(&data).unwrap();
        }
        zw.finish().unwrap();
        pdx_path
    }

    #[test]
    fn dir_candidates_glob_fallback_filters() {
        let t = make_dir_fixture("glob");
        t.write("notes.xml", "plain text, not xml");
        t.write(
            "index.xml",
            r#"<?xml version="1.0"?><CATALOG><SHORT-NAME>x</SHORT-NAME></CATALOG>"#,
        );
        let mut src = DirSource::new(t.0.clone());
        let cands = src.candidates().unwrap();
        let names: Vec<String> = cands.iter().map(|c| file_name_lower(c)).collect();
        assert_eq!(names, ["ext.xml", "main.xml"]);
    }

    #[test]
    fn dir_candidates_index_xml_locates_members() {
        let t = make_dir_fixture("idx");
        t.write(
            "index.xml",
            r#"<?xml version="1.0"?>
<CATALOG>
	<SHORT-NAME>pack</SHORT-NAME>
	<ABLOCKS>
		<ABLOCK UPD="NEW">
			<SHORT-NAME>main</SHORT-NAME>
			<CATEGORY>ODX-DATA</CATEGORY>
			<FILES>
				<FILE MIME-TYPE="text/xml">main.xml</FILE>
			</FILES>
		</ABLOCK>
	</ABLOCKS>
</CATALOG>
"#,
        );
        let mut src = DirSource::new(t.0.clone());
        let cands = src.candidates().unwrap();
        let names: Vec<String> = cands.iter().map(|c| file_name_lower(c)).collect();
        assert_eq!(names, ["main.xml"]);
    }

    #[test]
    fn open_path_main_and_available() {
        let t = make_dir_fixture("open");
        let set = OdxDocumentSet::open_path(t.0.join("MAIN.XML")).unwrap();
        assert!(set.main().source_file.is_some());
        assert_eq!(set.available_files().len(), 1);
        assert_eq!(file_name_lower(&set.available_files()[0]), "ext.xml");
        assert!(set.find_id("ESD.Main").is_some());
    }

    #[test]
    fn resolve_docref_lazy_load_success() {
        let t = make_dir_fixture("lazy");
        let mut set = OdxDocumentSet::open_path(t.0.join("main.xml")).unwrap();
        assert_eq!(set.external_references().len(), 0);
        let r = docref("DOP.Ext", "ext");
        let obj = set.resolve(&r).expect("cross-file resolve should succeed");
        assert_eq!(obj.id(), Some("DOP.Ext"));
        assert_eq!(set.available_files().len(), 0);
        assert_eq!(set.external_references().len(), 1);
        assert!(!set.failed_to_load(&r));
        assert!(set.find_id("DOP.Ext").is_some());
    }

    #[test]
    fn resolve_docref_case_insensitive_doc_name() {
        let t = make_dir_fixture("icase");
        let mut set = OdxDocumentSet::open_path(t.0.join("main.xml")).unwrap();
        let r = docref("DOP.Ext", "EXT");
        assert!(set.resolve(&r).is_some());
    }

    #[test]
    fn resolve_local_hit_and_miss() {
        let t = make_dir_fixture("local");
        let mut set = OdxDocumentSet::open_path(t.0.join("main.xml")).unwrap();
        let hit = IdRef::new("ESD.Main");
        assert!(set.resolve(&hit).is_some());
        let miss = IdRef::new("ESD.Nope");
        assert!(set.resolve(&miss).is_none());
        assert!(set.failed_to_load(&miss));
        assert!(!set.parser_events().is_empty());
    }

    #[test]
    fn resolve_docref_doc_loaded_id_missing() {
        let t = make_dir_fixture("noid");
        let mut set = OdxDocumentSet::open_path(t.0.join("main.xml")).unwrap();
        let r = docref("DOP.Missing", "ext");
        assert!(set.resolve(&r).is_none());
        assert!(set.failed_to_load(&r));
        assert_eq!(set.external_references().len(), 1);
    }

    #[test]
    fn resolve_docref_file_missing() {
        let t = make_dir_fixture("nodoc");
        let mut set = OdxDocumentSet::open_path(t.0.join("main.xml")).unwrap();
        let r = docref("DOP.Whatever", "nonexistent");
        assert!(set.resolve(&r).is_none());
        assert!(set.failed_to_load(&r));
        assert_eq!(set.available_files().len(), 1);
        assert!(set.resolve(&r).is_none());
    }

    #[test]
    fn resolve_docref_id_shadowing_quirk() {
        let t = TempDir::new("shadow");
        t.write("main.xml", MAIN_XML);
        t.write("ext.xml", EXT_XML);
        let mut set = OdxDocumentSet::open_path(t.0.join("main.xml")).unwrap();
        let r = docref("ESD.Main", "ext");
        let obj = set.resolve(&r).expect("id hit in already-loaded main doc");
        assert_eq!(obj.id(), Some("ESD.Main"));
        assert_eq!(set.external_references().len(), 0);
    }

    #[test]
    fn open_dir_autodetect_root_by_tag() {
        let t = TempDir::new("autodir");
        t.write(
            "aaa_flash.xml",
            r#"<?xml version="1.0"?>
<ODX MODEL-VERSION="2.2.0">
	<FLASH ID="FL.1" />
</ODX>
"#,
        );
        t.write("zzz_main.xml", MAIN_XML);
        let set = OdxDocumentSet::open_dir(&t.0).unwrap();
        assert_eq!(
            file_name_lower(set.main().source_file.as_deref().unwrap()),
            "zzz_main.xml"
        );
        assert_eq!(set.available_files().len(), 1);
    }

    #[test]
    fn pdx_members_and_index_location() {
        let t = make_dir_fixture("pdxidx");
        t.write(
            "index.xml",
            r#"<?xml version="1.0"?>
<CATALOG>
	<SHORT-NAME>pack</SHORT-NAME>
	<ABLOCKS>
		<ABLOCK UPD="NEW">
			<SHORT-NAME>docs</SHORT-NAME>
			<CATEGORY>ODX-DATA</CATEGORY>
			<FILES>
				<FILE MIME-TYPE="text/xml">main.xml</FILE>
				<FILE MIME-TYPE="text/xml">ext.xml</FILE>
			</FILES>
		</ABLOCK>
		<ABLOCK UPD="UNCHANGED">
			<SHORT-NAME>misc</SHORT-NAME>
			<CATEGORY>SUPPLEMENT</CATEGORY>
			<FILES>
				<FILE MIME-TYPE="text/plain">readme.txt</FILE>
			</FILES>
        </ABLOCK>
	</ABLOCKS>
</CATALOG>
"#,
        );
        t.write("readme.txt", "not odx");
        let pdx = make_pdx(&t, &["index.xml", "main.xml", "ext.xml", "readme.txt"]);
        let mut pkg = PdxPackage::open(&pdx).unwrap();
        assert_eq!(pkg.len(), 4);
        assert_eq!(
            pkg.member_names(),
            vec!["index.xml", "main.xml", "ext.xml", "readme.txt"]
        );
        let index = pkg.index_xml().unwrap().expect("index.xml present");
        assert_eq!(index.short_name.as_deref(), Some("pack"));
        assert_eq!(pkg.odx_member_names().unwrap(), ["main.xml", "ext.xml"]);
        assert!(pkg.read_member("main.xml").unwrap().contains("ESD.Main"));
    }

    #[test]
    fn pdx_odx_members_glob_fallback() {
        let t = make_dir_fixture("pdxglob");
        let pdx = make_pdx(&t, &["main.xml", "ext.xml"]);
        let mut pkg = PdxPackage::open(&pdx).unwrap();
        assert!(pkg.index_xml().unwrap().is_none());
        assert_eq!(pkg.odx_member_names().unwrap(), ["ext.xml", "main.xml"]);
    }

    #[test]
    fn open_pdx_cross_file_docref() {
        let t = make_dir_fixture("pdxopen");
        t.write(
            "index.xml",
            r#"<?xml version="1.0"?>
<CATALOG>
	<SHORT-NAME>pack</SHORT-NAME>
	<ABLOCKS>
		<ABLOCK UPD="NEW">
			<SHORT-NAME>docs</SHORT-NAME>
			<CATEGORY>ODX-DATA</CATEGORY>
			<FILES>
				<FILE MIME-TYPE="text/xml">main.xml</FILE>
				<FILE MIME-TYPE="text/xml">ext.xml</FILE>
			</FILES>
		</ABLOCK>
	</ABLOCKS>
</CATALOG>
"#,
        );
        let pdx = make_pdx(&t, &["index.xml", "main.xml", "ext.xml"]);
        let mut set = OdxDocumentSet::open_pdx(&pdx).unwrap();
        assert_eq!(
            file_name_lower(set.main().source_file.as_deref().unwrap()),
            "main.xml"
        );
        let r = docref("DOP.Ext", "ext");
        let obj = set.resolve(&r).expect("pdx cross-file resolve");
        assert_eq!(obj.id(), Some("DOP.Ext"));
        assert_eq!(set.available_files().len(), 0);
        assert_eq!(set.external_references().len(), 1);
        let miss = docref("DOP.Missing", "missing");
        assert!(set.resolve(&miss).is_none());
        assert!(set.failed_to_load(&miss));
    }
}
