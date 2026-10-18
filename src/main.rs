use std::path::PathBuf;
use std::process::ExitCode;

const USAGE: &str = "\
figit — a terminal Git client built around a real commit graph

USAGE:
    figit [OPTIONS] [PATH]

ARGS:
    <PATH>    Repository to open (defaults to the current directory)

OPTIONS:
    -h, --help       Print this help and exit
    -V, --version    Print version information and exit

KEYS:
    See README.md, or press Tab to cycle panes inside the app.
    q quits, Esc closes overlays, / searches commits, U checks/installs updates.
";

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("figit: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> anyhow::Result<()> {
    let mut path: Option<PathBuf> = None;
    for argument in std::env::args().skip(1) {
        match argument.as_str() {
            "-h" | "--help" => {
                print!("{USAGE}");
                return Ok(());
            }
            "-V" | "--version" => {
                println!("figit {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            _ if argument.starts_with('-') => {
                anyhow::bail!("unknown option '{argument}'\n\n{USAGE}");
            }
            _ => {
                if path.is_some() {
                    anyhow::bail!("expected a single repository path\n\n{USAGE}");
                }
                path = Some(PathBuf::from(argument));
            }
        }
    }

    let path = path.unwrap_or_else(|| std::env::current_dir().expect("cannot resolve current dir"));

    let backend = figit::backend::open(&path)?;
    let mut app = figit::ui::app::App::loading(backend);
    app.run()
}
