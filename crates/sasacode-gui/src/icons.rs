//! The window's icons: 16-unit line drawings, tinted where they are used.

use iced::widget::svg::{self, Svg};
use iced::{Color, Length};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    ChevronRight,
    ChevronLeft,
    ChevronDown,
    Check,
    Plus,
    File,
    Folder,
    Search,
    Terminal,
    Pencil,
    Sidebar,
    Panel,
    Alert,
    Stop,
    Arrow,
    Leaf,
    Shield,
    List,
    Gauge,
    Diff,
    Spark,
    Bulb,
    Bot,
    Circle,
    Dot,
    Half,
    Copy,
    Down,
    Close,
    Pin,
    Archive,
    Branch,
    Gear,
    Trash,
    Tag,
    Play,
}

impl Icon {
    fn body(self) -> &'static str {
        match self {
            Icon::ChevronRight => r#"<path d="M6 3.5 10.5 8 6 12.5"/>"#,
            Icon::ChevronLeft => r#"<path d="M10 3.5 5.5 8 10 12.5"/>"#,
            Icon::ChevronDown => r#"<path d="M3.5 6 8 10.5 12.5 6"/>"#,
            Icon::Check => r#"<path d="M3 8.5 6.5 12 13 4.5"/>"#,
            Icon::Plus => r#"<path d="M8 3v10M3 8h10"/>"#,
            Icon::File => r#"<path d="M4 2h5l3 3v9H4zM9 2v3h3"/>"#,
            Icon::Folder => r#"<path d="M2 4.5h4l1.5 1.5H14v6.5H2z"/>"#,
            Icon::Search => r#"<circle cx="7" cy="7" r="4.5"/><path d="M10.5 10.5 14 14"/>"#,
            Icon::Terminal => {
                r#"<rect x="2" y="3" width="12" height="10" rx="1.5"/><path d="M5 6.5 7 8.5 5 10.5M8.5 10.5H11"/>"#
            }
            Icon::Pencil => r#"<path d="M3 13l.7-2.8L10.9 3a1.4 1.4 0 0 1 2 2L5.8 12.3z"/>"#,
            Icon::Sidebar => r#"<rect x="2" y="3" width="12" height="10" rx="1.5"/><path d="M6 3v10"/>"#,
            Icon::Panel => r#"<rect x="2" y="3" width="12" height="10" rx="1.5"/><path d="M10 3v10"/>"#,
            Icon::Alert => r#"<path d="M8 2.5 14 13H2zM8 6.5v3M8 11.3v.1"/>"#,
            Icon::Stop => r##"<rect x="4" y="4" width="8" height="8" rx="1.5" fill="#000" stroke="none"/>"##,
            Icon::Arrow => r#"<path d="M3 8h10M9 4l4 4-4 4"/>"#,
            Icon::Leaf => r#"<path d="M2.5 13.5C2.5 7 6.5 2.5 13.5 2.5c0 7-4.5 11-11 11zM2.5 13.5l6-6"/>"#,
            Icon::Shield => r#"<path d="M8 2 13 4v4c0 3-2.2 5-5 6-2.8-1-5-3-5-6V4z"/>"#,
            Icon::List => r#"<path d="M6 4h7M6 8h7M6 12h7M3 4h.1M3 8h.1M3 12h.1"/>"#,
            Icon::Gauge => r#"<path d="M2.5 11a5.5 5.5 0 1 1 11 0M8 11l2.5-3.5"/>"#,
            Icon::Diff => r#"<path d="M4 5h5M6.5 2.5v5M4 11.5h5"/>"#,
            Icon::Spark => r#"<path d="M8 2.5v3M8 10.5v3M2.5 8h3M10.5 8h3"/>"#,
            Icon::Bulb => {
                r#"<path d="M6 11.5h4M6.5 13.5h3M5 9a4 4 0 1 1 6 0c-.6.7-1 1.3-1 2.5H6C6 10.3 5.6 9.7 5 9z"/>"#
            }
            Icon::Bot => r#"<rect x="3" y="5.5" width="10" height="7.5" rx="2"/><path d="M8 5.5V3M6 9h.1M10 9h.1"/>"#,
            Icon::Circle => r#"<circle cx="8" cy="8" r="5"/>"#,
            Icon::Dot => r##"<circle cx="8" cy="8" r="3.5" fill="#000" stroke="none"/>"##,
            Icon::Half => {
                r##"<circle cx="8" cy="8" r="5"/><path d="M8 3a5 5 0 0 1 0 10z" fill="#000" stroke="none"/>"##
            }
            Icon::Copy => {
                r#"<rect x="5.5" y="5.5" width="8" height="8" rx="1.5"/><path d="M10.5 3.5V3A1 1 0 0 0 9.5 2h-6A1 1 0 0 0 2.5 3v6a1 1 0 0 0 1 1h.5"/>"#
            }
            Icon::Down => r#"<path d="M8 3v10M4 9l4 4 4-4"/>"#,
            Icon::Close => r#"<path d="M4 4l8 8M12 4l-8 8"/>"#,
            Icon::Pin => r#"<path d="M6 2.5h4M7 2.5v4L4.5 9h7L9 6.5v-4M8 9v4.5"/>"#,
            Icon::Archive => r#"<rect x="2" y="3" width="12" height="3" rx="0.8"/><path d="M3 6v7h10V6M6.5 8.5h3"/>"#,
            Icon::Branch => {
                r#"<circle cx="5" cy="3.5" r="1.5"/><circle cx="5" cy="12.5" r="1.5"/><circle cx="11" cy="6" r="1.5"/><path d="M5 5v6M11 7.5c0 2-6 1.5-6 3.5"/>"#
            }
            Icon::Gear => {
                r#"<circle cx="8" cy="8" r="2.2"/><path d="M8 1.8v2M8 12.2v2M1.8 8h2M12.2 8h2M3.6 3.6 5 5M11 11l1.4 1.4M3.6 12.4 5 11M11 5l1.4-1.4"/>"#
            }
            Icon::Trash => r#"<path d="M2.5 4.5h11M6 4.5V3h4v1.5M4 4.5l.7 9h6.6l.7-9M6.8 7v4.5M9.2 7v4.5"/>"#,
            Icon::Tag => r#"<path d="M2.5 2.5h5.2l6 6-5.2 5.2-6-6zM5.3 5.3h.1"/>"#,
            Icon::Play => r#"<path d="M5 3.5v9l7-4.5z"/>"#,
        }
    }

    fn handle(self) -> svg::Handle {
        let source = format!(
            r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="none" stroke="#000" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round">{}</svg>"##,
            self.body()
        );
        svg::Handle::from_memory(source.into_bytes())
    }
}

/// An icon `size` pixels square in `color`.
pub fn icon<'a>(icon: Icon, size: f32, color: Color) -> Svg<'a> {
    Svg::new(icon.handle())
        .width(Length::Fixed(size))
        .height(Length::Fixed(size))
        .style(move |_, _| svg::Style { color: Some(color) })
}
