//! `sasacode-gui [folder]`: a desktop window for sasacode. Each prompt runs
//! `sasacode -p … --output jsonl --control stdio` in the folder, so the agent, its tools, its
//! permissions and its sessions are exactly those of the CLI and the TUI.

mod app;
mod diff;
#[cfg(test)]
mod drive;
mod i18n;
mod icons;
mod layout;
mod runner;
#[cfg(test)]
mod snapshots;
mod theme;
mod transcript;
mod view;

use std::path::PathBuf;

const USAGE: &str = "usage: sasacode-gui [--sasacode <path>] [--session <id>] [folder]

  folder              the project to work in (default: the current directory)
  --sasacode <path>   the sasacode executable (default: $SASACODE_BIN, else sasacode on PATH)
  --session <id>      open on this conversation of the folder (its id, or the start of it)
  -h, --help          show this
  -v, --version       show the version";

fn main() -> iced::Result {
    let mut program = std::env::var_os("SASACODE_BIN").map(PathBuf::from);
    let mut folder = None;
    let mut session = None;
    let mut args = std::env::args_os().skip(1);
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("-h" | "--help") => {
                println!("{USAGE}");
                return Ok(());
            }
            Some("-v" | "--version") => {
                println!("sasacode-gui {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            Some("--sasacode") => match args.next() {
                Some(path) => program = Some(PathBuf::from(path)),
                None => exit_with("--sasacode needs a path"),
            },
            Some("--session") => match args.next().map(|id| id.to_string_lossy().into_owned()) {
                Some(id) if !id.is_empty() => session = Some(id),
                _ => exit_with("--session needs an id"),
            },
            Some(flag) if flag.starts_with('-') => exit_with(&format!("unknown option {flag}")),
            _ => folder = Some(PathBuf::from(arg)),
        }
    }
    let cwd = folder
        .map(|f| std::path::absolute(&f).unwrap_or(f))
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    if !cwd.is_dir() {
        exit_with(&format!("{} is not a folder", cwd.display()));
    }
    let layout_file = sasacode_protocol::sasacode_home().map(|home| home.join(layout::FILE));
    let layout = layout_file.as_deref().map(layout::Layout::read).unwrap_or_default();
    let options = app::Options {
        program: program.unwrap_or_else(|| PathBuf::from("sasacode")),
        cwd,
        session,
        layout,
        layout_file,
    };

    iced::application(move || app::App::new(options.clone()), app::App::update, app::App::view)
        .title(app::App::title)
        .theme(app::App::theme)
        .subscription(app::App::subscription)
        .default_font(view::UI)
        .window(iced::window::Settings {
            size: layout.size,
            min_size: Some(layout::MIN_SIZE),
            ..iced::window::Settings::default()
        })
        .run()
}

fn exit_with(message: &str) -> ! {
    eprintln!("sasacode-gui: {message}\n\n{USAGE}");
    std::process::exit(2)
}
