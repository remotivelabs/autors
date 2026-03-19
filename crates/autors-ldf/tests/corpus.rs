use autors_ldf::model::Ldf;
use std::path::PathBuf;

#[test]
fn parses_and_round_trips_configured_corpus() {
    let Some(directory) = std::env::var_os("AUTORS_LDF_CORPUS").map(PathBuf::from) else {
        return;
    };
    let mut paths = std::fs::read_dir(&directory)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", directory.display()))
        .map(|entry| entry.expect("cannot read corpus entry").path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "ldf"))
        .collect::<Vec<_>>();
    paths.sort();
    assert!(!paths.is_empty(), "configured LDF corpus is empty");
    for path in paths {
        let first = Ldf::read(&path)
            .unwrap_or_else(|error| panic!("cannot parse {}: {error}", path.display()));
        let text = first
            .write_string()
            .unwrap_or_else(|error| panic!("cannot write {}: {error}", path.display()));
        let second = Ldf::parse_str(&text).unwrap_or_else(|error| {
            panic!("cannot parse generated {}: {error}\n{text}", path.display())
        });
        assert_eq!(first, second, "{}", path.display());
    }
}
