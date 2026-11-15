//! The window: a list of this folder's conversations on the left, the conversation in the middle,
//! the approval bar and the input below it.

use std::collections::HashSet;
use std::path::PathBuf;

use iced::font::Weight;
use iced::keyboard::{Key, key::Named};
use iced::widget::{
    Space, button, column, container, markdown, pick_list, row, rule, scrollable, text, text_editor, text_input,
};
use iced::{Alignment, Element, Font, Length, Subscription, Task, Theme, task, theme};
use sasacode_protocol::{ControlMessage, Decision, SessionSummary, list_sessions, read_entries, restore};
use tokio::sync::mpsc::UnboundedSender;

use crate::i18n::Strings;
use crate::runner::{self, RunSpec, RunUpdate};
use crate::transcript::{Item, Transcript};

const PERMISSIONS: [&str; 4] = ["edits", "ask", "agent", "auto"];
const EFFORTS: [&str; 6] = ["off", "low", "medium", "high", "xhigh", "max"];
const BOLD: Font = Font {
    weight: Weight::Bold,
    ..Font::DEFAULT
};

#[derive(Debug, Clone)]
pub struct Options {
    pub program: PathBuf,
    pub cwd: PathBuf,
}

pub struct App {
    s: Strings,
    program: PathBuf,
    cwd: PathBuf,
    home: Option<PathBuf>,
    theme: Theme,
    sessions: Vec<SessionSummary>,
    /// The session the next prompt continues; `None` starts a new one.
    session: Option<String>,
    transcript: Transcript,
    editor: text_editor::Content,
    feedback: String,
    model: String,
    permission: Option<String>,
    effort: Option<String>,
    /// Tool cards whose output is shown.
    open: HashSet<usize>,
    run: Option<Run>,
}

struct Run {
    control: Option<UnboundedSender<ControlMessage>>,
    _handle: task::Handle,
    stderr: Vec<String>,
    ended: bool,
}

#[derive(Debug, Clone)]
pub enum Msg {
    Edit(text_editor::Action),
    Send,
    Stop,
    Run(RunUpdate),
    Answer(Decision),
    Feedback(String),
    NewSession,
    OpenSession(PathBuf),
    Model(String),
    Permission(String),
    Effort(String),
    Toggle(usize),
    Link(markdown::Uri),
    ThemeMode(theme::Mode),
}

impl App {
    pub fn new(options: Options) -> (Self, Task<Msg>) {
        let home = sasacode_protocol::sasacode_home();
        let mut app = App {
            s: Strings::detect(),
            program: options.program,
            cwd: options.cwd,
            home,
            theme: Theme::Light,
            sessions: Vec::new(),
            session: None,
            transcript: Transcript::default(),
            editor: text_editor::Content::new(),
            feedback: String::new(),
            model: String::new(),
            permission: None,
            effort: None,
            open: HashSet::new(),
            run: None,
        };
        app.reload_sessions();
        (app, iced::system::theme().map(Msg::ThemeMode))
    }

    pub fn title(&self) -> String {
        let folder = self
            .cwd
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_default();
        format!("sasacode — {folder}")
    }

    pub fn theme(&self) -> Theme {
        self.theme.clone()
    }

    pub fn subscription(&self) -> Subscription<Msg> {
        iced::system::theme_changes().map(Msg::ThemeMode)
    }

    fn reload_sessions(&mut self) {
        self.sessions = match &self.home {
            Some(home) => list_sessions(home, &self.cwd),
            None => Vec::new(),
        };
    }

    fn running(&self) -> bool {
        self.run.is_some()
    }

    fn control(&self, message: ControlMessage) {
        if let Some(tx) = self.run.as_ref().and_then(|r| r.control.as_ref()) {
            let _ = tx.send(message);
        }
    }

    pub fn update(&mut self, msg: Msg) -> Task<Msg> {
        match msg {
            Msg::Edit(action) => self.editor.perform(action),
            Msg::Send => return self.send(),
            Msg::Stop => self.control(ControlMessage::Abort),
            Msg::Run(update) => self.on_run(update),
            Msg::Answer(decision) => {
                if let Some(approval) = self.transcript.pending_approval() {
                    let id = approval.id.clone();
                    let feedback = (decision == Decision::Deny && !self.feedback.trim().is_empty())
                        .then(|| self.feedback.trim().to_owned());
                    let said = match decision {
                        Decision::Allow => self.s.allowed,
                        Decision::Always => self.s.always_allowed,
                        Decision::Deny => self.s.denied,
                    };
                    self.control(ControlMessage::Approval {
                        id: id.clone(),
                        decision,
                        feedback,
                    });
                    self.transcript.answer_approval(&id, said.into());
                    self.feedback.clear();
                }
            }
            Msg::Feedback(text) => self.feedback = text,
            Msg::NewSession => {
                if !self.running() {
                    self.session = None;
                    self.transcript = Transcript::default();
                    self.open.clear();
                }
            }
            Msg::OpenSession(path) => {
                if !self.running()
                    && let Ok(entries) = read_entries(&path)
                {
                    let restored = restore(&entries);
                    self.transcript = Transcript::from_messages(&restored.messages);
                    self.session = self.sessions.iter().find(|s| s.path == path).map(|s| s.id.clone());
                    self.open.clear();
                }
            }
            Msg::Model(model) => self.model = model,
            Msg::Permission(p) => self.permission = Some(p),
            Msg::Effort(e) => self.effort = Some(e),
            Msg::Toggle(i) => {
                if !self.open.remove(&i) {
                    self.open.insert(i);
                }
            }
            Msg::Link(uri) => open_link(&uri),
            Msg::ThemeMode(mode) => {
                self.theme = if mode == theme::Mode::Dark {
                    Theme::Dark
                } else {
                    Theme::Light
                };
            }
        }
        Task::none()
    }

    fn send(&mut self) -> Task<Msg> {
        let prompt = self.editor.text().trim().to_owned();
        if prompt.is_empty() {
            return Task::none();
        }
        self.editor = text_editor::Content::new();
        self.transcript.send(&prompt);
        if self.running() {
            // Like typing while the TUI is busy: it reaches the model at its next turn.
            self.control(ControlMessage::Message { text: prompt });
            return Task::none();
        }
        let model = self.model.trim();
        let spec = RunSpec {
            program: self.program.clone(),
            cwd: self.cwd.clone(),
            prompt,
            resume: self.session.clone(),
            model: (!model.is_empty()).then(|| model.to_owned()),
            permission: self.permission.clone(),
            thinking: self.effort.clone(),
        };
        let (task, handle) = Task::run(runner::run(spec), Msg::Run).abortable();
        self.run = Some(Run {
            control: None,
            _handle: handle.abort_on_drop(),
            stderr: Vec::new(),
            ended: false,
        });
        task
    }

    fn on_run(&mut self, update: RunUpdate) {
        let Some(run) = self.run.as_mut() else { return };
        match update {
            RunUpdate::Started(control) => run.control = Some(control),
            RunUpdate::Event(event) => {
                let effect = self.transcript.apply(&event, &self.s);
                if let Some(id) = effect.session_id {
                    self.session = Some(id);
                }
                if effect.ended.is_some() {
                    run.ended = true;
                }
            }
            RunUpdate::Stderr(line) => {
                run.stderr.push(line);
                if run.stderr.len() > 40 {
                    run.stderr.remove(0);
                }
            }
            RunUpdate::Failed(error) => {
                self.transcript.notice(format!("{}\n{error}", self.s.not_found), true);
                self.run = None;
            }
            RunUpdate::Exited(code) => {
                // A run that never got to its end says why only on stderr.
                if !run.ended {
                    let code = code.map(|c| c.to_string()).unwrap_or_else(|| "?".into());
                    let mut text = self.s.exited.replace("{code}", &code);
                    let tail = run.stderr.join("\n");
                    if !tail.trim().is_empty() {
                        text = format!("{text}\n{tail}");
                    }
                    self.transcript.notice(text, true);
                }
                self.run = None;
                self.reload_sessions();
            }
        }
    }

    pub fn view(&self) -> Element<'_, Msg> {
        row![self.sidebar(), rule::vertical(1), self.main()]
            .height(Length::Fill)
            .into()
    }

    fn sidebar(&self) -> Element<'_, Msg> {
        let mut list = column![].spacing(4);
        if self.sessions.is_empty() {
            list = list.push(text(self.s.no_sessions).size(13).style(text::secondary));
        }
        for s in &self.sessions {
            let title: String = if s.first_prompt.is_empty() {
                s.id.clone()
            } else {
                s.first_prompt.clone()
            };
            let title: String = title.chars().take(28).collect();
            let when = s.created_at.get(..16).unwrap_or(&s.created_at).replace('T', " ");
            let current = self.session.as_deref() == Some(s.id.as_str());
            list = list.push(
                button(column![text(title).size(14), text(when).size(11).style(text::secondary)].spacing(2))
                    .width(Length::Fill)
                    .style(if current { button::subtle } else { button::text })
                    .on_press_maybe((!self.running()).then(|| Msg::OpenSession(s.path.clone()))),
            );
        }
        container(
            column![
                button(text(self.s.new_session))
                    .width(Length::Fill)
                    .on_press_maybe((!self.running()).then_some(Msg::NewSession)),
                text(self.s.sessions).size(12).style(text::secondary),
                scrollable(list).height(Length::Fill),
            ]
            .spacing(10),
        )
        .padding(12)
        .width(250)
        .into()
    }

    fn main(&self) -> Element<'_, Msg> {
        let mut body = column![self.header(), rule::horizontal(1), self.conversation()];
        if let Some(bar) = self.approval_bar() {
            body = body.push(bar);
        }
        body.push(self.input()).push(self.status()).width(Length::Fill).into()
    }

    fn header(&self) -> Element<'_, Msg> {
        let permission = pick_list(PERMISSIONS.map(String::from), self.permission.clone(), Msg::Permission)
            .placeholder(self.s.permission)
            .text_size(13);
        let effort = pick_list(EFFORTS.map(String::from), self.effort.clone(), Msg::Effort)
            .placeholder(self.s.effort)
            .text_size(13);
        container(
            row![
                text(short_path(&self.cwd))
                    .size(13)
                    .style(text::secondary)
                    .width(Length::Fill),
                text_input(self.s.model_default, &self.model)
                    .on_input(Msg::Model)
                    .size(13)
                    .width(230),
                permission,
                effort,
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        )
        .padding([8, 16])
        .into()
    }

    fn conversation(&self) -> Element<'_, Msg> {
        if self.transcript.items.is_empty() {
            return container(text(self.s.empty).style(text::secondary))
                .center(Length::Fill)
                .into();
        }
        let items = self
            .transcript
            .items
            .iter()
            .enumerate()
            .map(|(i, item)| self.item(i, item));
        scrollable(
            container(column(items).spacing(12).max_width(860))
                .padding([16, 24])
                .center_x(Length::Fill),
        )
        .anchor_bottom()
        .height(Length::Fill)
        .into()
    }

    fn item<'a>(&'a self, i: usize, item: &'a Item) -> Element<'a, Msg> {
        match item {
            Item::User(t) => container(text(t.as_str()))
                .padding([8, 12])
                .style(container::rounded_box)
                .width(Length::Fill)
                .into(),
            Item::Assistant(md) => {
                markdown::view(md.content.items(), markdown::Settings::with_text_size(15, &self.theme)).map(Msg::Link)
            }
            Item::Thinking(t) => {
                let open = self.open.contains(&i);
                let head = button(text(format!("{} {}", if open { "▾" } else { "▸" }, self.s.thinking)).size(13))
                    .style(button::text)
                    .padding(0)
                    .on_press(Msg::Toggle(i));
                let mut c = column![head].spacing(4);
                if open {
                    c = c.push(text(t.as_str()).size(13).style(text::secondary));
                }
                c.into()
            }
            Item::Tool(tool) => {
                let open = self.open.contains(&i);
                let (mark, style): (&str, fn(&Theme) -> text::Style) = match tool.is_error {
                    None => ("●", text::secondary),
                    Some(false) => ("✓", text::success),
                    Some(true) => ("✗", text::danger),
                };
                let head = row![
                    text(mark).style(style),
                    text(tool.name.as_str()).font(BOLD).size(14),
                    text(tool.summary.as_str()).font(Font::MONOSPACE).size(13),
                ]
                .spacing(8)
                .align_y(Alignment::Center);
                let mut c = column![button(head).style(button::text).padding(0).on_press(Msg::Toggle(i))].spacing(6);
                if open && !tool.output.is_empty() {
                    c = c.push(text(tool.output.as_str()).font(Font::MONOSPACE).size(12));
                }
                container(c)
                    .padding([8, 12])
                    .style(container::bordered_box)
                    .width(Length::Fill)
                    .into()
            }
            Item::Approval(a) => {
                let state = a.answer.as_deref().unwrap_or("…");
                text(format!("{}: {} — {state}", a.tool, first_line(&a.detail)))
                    .size(13)
                    .style(text::secondary)
                    .into()
            }
            Item::Notice { text: t, error } => text(t.as_str())
                .size(13)
                .style(if *error { text::danger } else { text::secondary })
                .into(),
        }
    }

    fn approval_bar(&self) -> Option<Element<'_, Msg>> {
        let a = self.transcript.pending_approval()?;
        let bar = column![
            text(format!("{} {}", a.tool, self.s.approve_question)).font(BOLD),
            container(text(a.detail.as_str()).font(Font::MONOSPACE).size(13))
                .padding(8)
                .style(container::rounded_box)
                .width(Length::Fill),
            text(a.reason.as_str()).size(12).style(text::secondary),
            row![
                text_input(self.s.feedback, &self.feedback)
                    .on_input(Msg::Feedback)
                    .on_submit(Msg::Answer(Decision::Deny)),
                button(text(self.s.allow))
                    .style(button::primary)
                    .on_press(Msg::Answer(Decision::Allow)),
                button(text(self.s.always))
                    .style(button::secondary)
                    .on_press(Msg::Answer(Decision::Always)),
                button(text(self.s.deny))
                    .style(button::danger)
                    .on_press(Msg::Answer(Decision::Deny)),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        ]
        .spacing(8);
        Some(
            container(bar)
                .padding(12)
                .style(container::bordered_box)
                .width(Length::Fill)
                .into(),
        )
    }

    fn input(&self) -> Element<'_, Msg> {
        let placeholder = if self.running() {
            self.s.placeholder_running
        } else {
            self.s.placeholder
        };
        let editor = text_editor(&self.editor)
            .placeholder(placeholder)
            .on_action(Msg::Edit)
            .min_height(56)
            .max_height(220)
            .key_binding(|press| {
                if press.key == Key::Named(Named::Enter) && !press.modifiers.shift() && !press.modifiers.alt() {
                    return Some(text_editor::Binding::Custom(Msg::Send));
                }
                text_editor::Binding::from_key_press(press)
            });
        let mut buttons = column![button(text(self.s.send)).style(button::primary).on_press(Msg::Send)].spacing(6);
        if self.running() {
            buttons = buttons.push(button(text(self.s.stop)).style(button::danger).on_press(Msg::Stop));
        }
        container(row![editor, buttons].spacing(8).align_y(Alignment::End))
            .padding([8, 16])
            .into()
    }

    fn status(&self) -> Element<'_, Msg> {
        let u = &self.transcript.usage;
        let usage = format!(
            "in {} · out {} · cache {} · ${:.4}",
            u.input, u.output, u.cache_read, u.cost
        );
        let state = if self.running() { self.s.running } else { self.s.idle };
        let session = self.session.as_deref().unwrap_or("");
        container(
            row![
                text(state).size(12),
                Space::new().width(Length::Fill),
                text(usage).size(12).style(text::secondary),
                text(session).size(12).font(Font::MONOSPACE).style(text::secondary),
            ]
            .spacing(16),
        )
        .padding([4, 16])
        .into()
    }
}

/// The folder's path, its start cut so the header keeps room for the controls.
fn short_path(path: &std::path::Path) -> String {
    let full = path.to_string_lossy();
    let chars: Vec<char> = full.chars().collect();
    if chars.len() <= 48 {
        return full.into_owned();
    }
    format!("…{}", chars[chars.len() - 47..].iter().collect::<String>())
}

fn first_line(s: &str) -> &str {
    s.lines().next().unwrap_or("")
}

/// Opens a link from a reply in the system's browser. Only web links: a reply is model output.
fn open_link(uri: &str) {
    if !(uri.starts_with("https://") || uri.starts_with("http://")) {
        return;
    }
    let mut cmd = if cfg!(target_os = "macos") {
        std::process::Command::new("open")
    } else if cfg!(windows) {
        let mut c = std::process::Command::new("rundll32");
        c.arg("url.dll,FileProtocolHandler");
        c
    } else {
        std::process::Command::new("xdg-open")
    };
    let _ = cmd.arg(uri).spawn();
}
