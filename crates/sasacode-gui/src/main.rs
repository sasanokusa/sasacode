//! `sasacode-gui [folder]`: a desktop window for sasacode. Each prompt runs
//! `sasacode -p … --output jsonl --control stdio` in the folder, so the agent, its tools, its
//! permissions and its sessions are exactly those of the CLI and the TUI.

mod app;
mod i18n;
mod icon;
mod palette;
mod prefs;
mod runner;
mod style;
mod transcript;
mod view;

use std::path::PathBuf;

const USAGE: &str = "usage: sasacode-gui [--sasacode <path>] [folder]

  folder              the project to work in (default: the current directory)
  --sasacode <path>   the sasacode executable (default: $SASACODE_BIN, else sasacode on PATH)
  -h, --help          show this
  -v, --version       show the version";

fn main() -> iced::Result {
    let mut program = std::env::var_os("SASACODE_BIN").map(PathBuf::from);
    let mut folder = None;
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
    // On macOS the window's own buttons sit on its content, with no bar of their own above it.
    // `SASACODE_TITLEBAR=system` brings the system's title bar back.
    let unified = cfg!(target_os = "macos") && std::env::var("SASACODE_TITLEBAR").as_deref() != Ok("system");
    let options = app::Options {
        program: program.unwrap_or_else(|| PathBuf::from("sasacode")),
        cwd,
        unified,
    };

    iced::application(move || app::App::new(options.clone()), app::App::update, app::App::view)
        .title(app::App::title)
        .theme(app::App::theme)
        .subscription(app::App::subscription)
        .font(icon::BYTES)
        .default_font(style::UI)
        .window(window(unified))
        .run()
}

/// The size the window opens with.
pub(crate) const WINDOW: iced::Size = iced::Size::new(1120.0, 780.0);

fn window(unified: bool) -> iced::window::Settings {
    #[cfg(target_os = "macos")]
    let platform_specific = iced::window::settings::PlatformSpecific {
        title_hidden: unified,
        titlebar_transparent: unified,
        fullsize_content_view: unified,
    };
    #[cfg(not(target_os = "macos"))]
    let platform_specific = {
        let _ = unified;
        iced::window::settings::PlatformSpecific::default()
    };
    iced::window::Settings {
        size: WINDOW,
        min_size: Some(iced::Size::new(560.0, 420.0)),
        platform_specific,
        ..iced::window::Settings::default()
    }
}

fn exit_with(message: &str) -> ! {
    eprintln!("sasacode-gui: {message}\n\n{USAGE}");
    std::process::exit(2)
}
