use std::path::PathBuf;

use autors_blf::file::BlfFile;

fn inspect(path: &std::path::Path, failed: &mut bool, quiet: bool) {
    if path.is_dir() {
        match std::fs::read_dir(path) {
            Ok(entries) => {
                for entry in entries {
                    match entry {
                        Ok(entry) => inspect(&entry.path(), failed, quiet),
                        Err(error) => {
                            eprintln!("{}: {error}", path.display());
                            *failed = true;
                        }
                    }
                }
            }
            Err(error) => {
                eprintln!("{}: {error}", path.display());
                *failed = true;
            }
        }
        return;
    }
    if path.extension().is_none_or(|extension| extension != "blf") {
        return;
    }
    match BlfFile::open(path) {
        Ok(file) => {
            let roundtrip = file
                .write()
                .and_then(|bytes| BlfFile::parse(&bytes))
                .map(|parsed| parsed.objects == file.objects);
            if !quiet {
                println!(
                    "{}: {} objects, {:?}, roundtrip={roundtrip:?}",
                    path.display(),
                    file.objects.len(),
                    file.statistics()
                );
            }
            if !matches!(roundtrip, Ok(true)) {
                *failed = true;
            }
        }
        Err(error) => {
            eprintln!("{}: {error}", path.display());
            *failed = true;
        }
    }
}

fn main() {
    let mut failed = false;
    let mut quiet = false;
    for path in std::env::args_os().skip(1).map(PathBuf::from) {
        if path.as_os_str() == "--quiet" {
            quiet = true;
        } else {
            inspect(&path, &mut failed, quiet);
        }
    }
    if failed {
        std::process::exit(1);
    }
}
