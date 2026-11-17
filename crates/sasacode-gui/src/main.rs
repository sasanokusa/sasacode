//! `sasacode-gui [folder]`: a desktop window for sasacode. Each prompt runs
//! `sasacode -p … --output jsonl --control stdio` in the folder, so the agent, its tools, its
//! permissions and its sessions are exactly those of the CLI and the TUI.

mod app;
mod composer;
mod diff;
#[cfg(test)]
mod drive;
mod export;
mod extras;
mod figure;
mod files;
mod fork;
mod git;
mod helper;
mod i18n;
mod icons;
mod input;
mod keys;
mod layout;
mod list;
mod outline;
mod overlay;
mod review;
mod rows;
mod runner;
mod scope;
mod scripts;
#[cfg(test)]
mod snapshots;
mod split;
mod system;
mod theme;
mod transcript;
mod view;
mod words;

use std::path::PathBuf;

const USAGE: &str = "usage: sasacode-gui [--sasacode <path>] [--session <id>] [folder]

  folder              the project to work in (default: the current directory)
  --sasacode <path>   the sasacode executable (default: $SASACODE_BIN, else sasacode on PATH)
  --session <id>      open on this conversation of the folder (its id, or the start of it)
  --at <x,y>          put the window's top left corner here, in points (a window opening another)
  -h, --help          show this
  -v, --version       show the version";

fn main() -> iced::Result {
    let mut program = std::env::var_os("SASACODE_BIN").map(PathBuf::from);
    let mut folder = None;
    let mut session = None;
    let mut at = None;
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
            Some("--at") => match args.next().as_deref().and_then(|at| at.to_str()).and_then(point) {
                Some(p) => at = Some(p),
                None => exit_with("--at needs x,y"),
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
    let prefs = layout_file.as_deref().map(layout::Prefs::read).unwrap_or_default();
    let zoom = prefs.zoom;
    let options = app::Options {
        program: program.unwrap_or_else(|| PathBuf::from("sasacode")),
        cwd,
        session,
        layout,
        prefs,
        layout_file,
    };

    iced::application(move || app::App::new(options.clone()), app::App::update, app::App::view)
        .title(app::App::title)
        .theme(app::App::theme)
        .subscription(app::App::subscription)
        .scale_factor(app::App::scale)
        .default_font(view::UI)
        .window(iced::window::Settings {
            // What the system measures, as iced takes it: before the zoom it draws at.
            size: iced::Size::new(layout.size.width / zoom, layout.size.height / zoom),
            min_size: Some(layout::MIN_SIZE),
            position: at.map_or(iced::window::Position::Default, iced::window::Position::Specific),
            // Asked first while something runs: `Msg::CloseAsked`.
            exit_on_close_request: false,
            ..iced::window::Settings::default()
        })
        .run()
}

/// `x,y`, as a point.
fn point(text: &str) -> Option<iced::Point> {
    let (x, y) = text.split_once(',')?;
    let (x, y) = (x.trim().parse::<f32>().ok()?, y.trim().parse::<f32>().ok()?);
    (x.is_finite() && y.is_finite()).then(|| iced::Point::new(x, y))
}

fn exit_with(message: &str) -> ! {
    eprintln!("sasacode-gui: {message}\n\n{USAGE}");
    std::process::exit(2)
}
