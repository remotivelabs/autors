mod a2l_lab;
mod app;
mod bus;
mod catalog;
mod document;
mod hardware;
mod lin_bus;
mod odx_lab;
mod prm_lab;
mod protocol;
mod symbol_lab;
mod ui;

use std::error::Error;
use std::io::{self, stdout};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use app::App;
use catalog::CapabilityCatalog;
use crossterm::event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

const HELP: &str = "autors-cli — terminal automotive engineering workbench

USAGE:
    autors-cli [OPTIONS] [FILE]

OPTIONS:
    --workspace <PATH>      autors workspace directory or Cargo.toml
    --manifest-path <PATH>  explicit workspace Cargo.toml
    -h, --help              print this help

FILES:
    A2L, DBC, LDF, ASC, BLF, LTRC, MDF, CDF/CDFX, ODX, PRM/CNF, ELF/MAP and
    common ECU program images receive native structured views. Other text and
    binary automotive files open in the universal inspector.

WORKBENCH:
    Press a for retained A2L measurement, calibration, and virtual DAQ.
    Press b for CAN/CAN FD transmit, tracing, DBC decoding, and scheduling.
    Press n for the equivalent LDF-driven LIN workbench. Virtual adapters are
    always present; cargo features enable Vector, Kvaser, and PEAK hardware.
    Press g for live and offline UDS/KWP, DoIP, CCP, and XCP workflows.
    Press d for an ODX-driven ECU variant/service diagnostic workbench.
";

fn main() -> Result<(), Box<dyn Error>> {
    let Some(options) = Options::parse(std::env::args().skip(1))? else {
        print!("{HELP}");
        return Ok(());
    };
    let catalog = CapabilityCatalog::load(&options.manifest_path).map_err(io::Error::other)?;
    let mut app = App::new(catalog);
    if let Some(path) = options.file {
        app.open_document(path);
    }
    run_terminal(&mut app)?;
    Ok(())
}

fn run_terminal(app: &mut App) -> io::Result<()> {
    enable_raw_mode()?;
    let mut output = stdout();
    execute!(output, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(output);
    let mut terminal = Terminal::new(backend)?;
    let result = event_loop(&mut terminal, app);
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;
    result
}

fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    app: &mut App,
) -> io::Result<()> {
    let mut previous_tick = Instant::now();
    while !app.should_quit {
        terminal.draw(|frame| ui::draw(frame, app))?;
        if event::poll(Duration::from_millis(33))? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    app.handle_key(key);
                }
            }
        }
        let now = Instant::now();
        app.advance_playback(now.duration_since(previous_tick));
        previous_tick = now;
    }
    Ok(())
}

#[derive(Debug)]
struct Options {
    manifest_path: PathBuf,
    file: Option<PathBuf>,
}

impl Options {
    fn parse(arguments: impl IntoIterator<Item = String>) -> Result<Option<Self>, String> {
        let mut arguments = arguments.into_iter();
        let mut manifest_path = default_manifest_path();
        let mut file = None;
        while let Some(argument) = arguments.next() {
            match argument.as_str() {
                "-h" | "--help" => return Ok(None),
                "--workspace" => {
                    let value = arguments
                        .next()
                        .ok_or_else(|| "--workspace requires a path".to_owned())?;
                    let path = PathBuf::from(value);
                    manifest_path = if path.file_name().is_some_and(|name| name == "Cargo.toml") {
                        path
                    } else {
                        path.join("Cargo.toml")
                    };
                }
                "--manifest-path" => {
                    manifest_path = PathBuf::from(
                        arguments
                            .next()
                            .ok_or_else(|| "--manifest-path requires a path".to_owned())?,
                    );
                }
                value if value.starts_with('-') => {
                    return Err(format!("unknown option {value:?}\n\n{HELP}"));
                }
                value if file.is_none() => file = Some(PathBuf::from(value)),
                value => return Err(format!("unexpected second file argument {value:?}")),
            }
        }
        Ok(Some(Self {
            manifest_path,
            file,
        }))
    }
}

fn default_manifest_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("Cargo.toml")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_accept_workspace_and_initial_file() {
        let options = Options::parse([
            "--workspace".to_owned(),
            "repo".to_owned(),
            "trace.asc".to_owned(),
        ])
        .unwrap()
        .unwrap();
        assert_eq!(
            options.manifest_path,
            PathBuf::from("repo").join("Cargo.toml")
        );
        assert_eq!(options.file, Some(PathBuf::from("trace.asc")));
    }

    #[test]
    fn help_short_circuits_startup() {
        assert!(Options::parse(["--help".to_owned()]).unwrap().is_none());
    }
}
