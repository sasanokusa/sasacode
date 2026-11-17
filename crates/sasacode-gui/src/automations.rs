//! Automations: a message the window sends on a schedule, each time in a new conversation of the
//! folder, beside the one shown. They are the window's, not the system's: they run while a window
//! of their folder is open, and one missed while none was runs once when one opens. Nothing is
//! registered with the system to start the window.
//!
//! What is due is a pure function of the schedule, when it was last run (or made) and the time
//! now, in the local time zone. Before a run starts, its time is written down, in the file other
//! windows read too: a second window of the folder finds it run.

use chrono::{Datelike, Duration, Local, NaiveDateTime, NaiveTime, TimeZone, Weekday};
use iced::Task;
use iced::widget::text_editor;

use crate::app::{App, Msg};
use crate::i18n::fill;
use crate::overlay::Overlay;

/// The permission modes an automation may run in: `auto` is chosen each time, by the user, and
/// never left to a schedule.
pub const MODES: [&str; 4] = ["plan", "ask", "edits", "agent"];

/// How often automations are looked at.
pub const LOOK_EVERY: std::time::Duration = std::time::Duration::from_secs(30);

/// When an automation runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum Every {
    /// Every so many hours, from the last run.
    Hours { hours: u32 },
    /// Each day, at so many minutes after midnight.
    Daily { at: u32 },
    /// Monday to Friday, at so many minutes after midnight.
    Weekdays { at: u32 },
}

impl Default for Every {
    fn default() -> Self {
        Every::Daily { at: 9 * 60 }
    }
}

/// The kinds of schedule, as the form offers them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Kind {
    Hours,
    #[default]
    Daily,
    Weekdays,
}

/// An automation, as the window keeps it.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Automation {
    pub name: String,
    /// The folder it runs in, by its path as the window names it.
    pub folder: String,
    pub prompt: String,
    pub every: Every,
    /// One of [`MODES`].
    pub mode: String,
    pub on: bool,
    /// When it was made, changed or turned on, in seconds since 1970: what is due is counted from
    /// then, or from its last run.
    pub since: i64,
    /// When its last run started.
    pub last: Option<i64>,
}

impl Default for Automation {
    fn default() -> Self {
        Automation {
            name: String::new(),
            folder: String::new(),
            prompt: String::new(),
            every: Every::default(),
            mode: MODES[0].into(),
            on: true,
            since: 0,
            last: None,
        }
    }
}

impl Automation {
    /// What its next run is counted from: its last run, or when it was made or turned on.
    fn from(&self) -> i64 {
        self.last.map_or(self.since, |last| last.max(self.since))
    }

    /// Its mode, as one of [`MODES`]; anything else is the first.
    pub fn mode(&self) -> &'static str {
        MODES.iter().copied().find(|m| *m == self.mode).unwrap_or(MODES[0])
    }

    /// Whether it should run at `now`.
    pub fn due(&self, now: i64) -> bool {
        self.on && due(self.every, local(self.from()), local(now))
    }

    /// When it runs next, as of `now`: `now` itself when it is due.
    pub fn next(&self, now: i64) -> NaiveDateTime {
        next(self.every, local(self.from()), local(now))
    }
}

/// `secs` since 1970 as the local time.
pub fn local(secs: i64) -> NaiveDateTime {
    Local
        .timestamp_opt(secs, 0)
        .earliest()
        .map(|t| t.naive_local())
        .unwrap_or_default()
}

/// Whether `every`, counted from `from`, has come round by `now`. A daily time missed, however
/// long ago, is due once.
pub fn due(every: Every, from: NaiveDateTime, now: NaiveDateTime) -> bool {
    match every {
        Every::Hours { hours } => now >= from + Duration::hours(i64::from(hours.max(1))),
        Every::Daily { .. } | Every::Weekdays { .. } => latest(every, now).is_some_and(|t| t > from),
    }
}

/// When `every`, counted from `from`, comes round next after `now`; `now` when it is due.
pub fn next(every: Every, from: NaiveDateTime, now: NaiveDateTime) -> NaiveDateTime {
    if due(every, from, now) {
        return now;
    }
    match every {
        Every::Hours { hours } => from + Duration::hours(i64::from(hours.max(1))),
        Every::Daily { at } | Every::Weekdays { at } => (0..8)
            .filter_map(|ahead| slot(every, now.date() + Duration::days(ahead), at))
            .find(|t| *t > now)
            .unwrap_or(now),
    }
}

/// The last time a daily `every` came round, at or before `now`.
fn latest(every: Every, now: NaiveDateTime) -> Option<NaiveDateTime> {
    let (Every::Daily { at } | Every::Weekdays { at }) = every else {
        return None;
    };
    (0..8)
        .filter_map(|back| slot(every, now.date() - Duration::days(back), at))
        .find(|t| *t <= now)
}

/// The time on `day` a daily `every` runs at, if it runs that day.
fn slot(every: Every, day: chrono::NaiveDate, at: u32) -> Option<NaiveDateTime> {
    let weekend = matches!(day.weekday(), Weekday::Sat | Weekday::Sun);
    if matches!(every, Every::Weekdays { .. }) && weekend {
        return None;
    }
    NaiveTime::from_hms_opt(at / 60 % 24, at % 60, 0).map(|t| day.and_time(t))
}

/// `09:00` as minutes after midnight.
pub fn parse_at(text: &str) -> Option<u32> {
    let (h, m) = text.trim().split_once([':', '：'])?;
    let (h, m): (u32, u32) = (h.trim().parse().ok()?, m.trim().parse().ok()?);
    (h < 24 && m < 60).then_some(h * 60 + m)
}

/// Minutes after midnight as `09:00`.
pub fn show_at(at: u32) -> String {
    format!("{:02}:{:02}", at / 60 % 24, at % 60)
}

/// The form an automation is written in.
#[derive(Debug, Default)]
pub struct Form {
    /// The automation being changed, by its id; none makes a new one.
    pub editing: Option<String>,
    pub name: String,
    pub prompt: text_editor::Content,
    pub kind: Kind,
    pub hours: String,
    pub at: String,
    pub mode: &'static str,
    /// What is wrong with what is written, said under it.
    pub wrong: Option<&'static str>,
}

impl Form {
    fn blank() -> Form {
        Form {
            hours: "3".into(),
            at: "09:00".into(),
            mode: MODES[0],
            ..Form::default()
        }
    }
}

/// What the automations' screen and their clock say.
#[derive(Debug, Clone)]
pub enum AutoMsg {
    Name(String),
    Prompt(text_editor::Action),
    Kind(Kind),
    Hours(String),
    At(String),
    Mode(&'static str),
    Save,
    /// The form put back to a new automation.
    Clear,
    Edit(String),
    Toggle(String),
    RunNow(String),
    Delete(String),
    /// Time to look at what is due.
    Look,
}

/// Seconds since 1970, now.
fn now_secs() -> i64 {
    Local::now().timestamp()
}

impl App {
    /// The automations of this window's folder, by id, in the order they were made.
    pub(crate) fn automations_here(&self) -> Vec<(&String, &Automation)> {
        let here = self.folder_key();
        self.prefs
            .more
            .automations
            .iter()
            .filter(|(_, a)| a.folder == here)
            .collect()
    }

    /// Whether any automation of this folder is on: the window then looks at the time.
    pub(crate) fn watches_automations(&self) -> bool {
        self.automations_here().iter().any(|(_, a)| a.on)
    }

    /// Whether a run of automation `id` still goes on: the next waits for it.
    fn automation_running(&self, id: &str) -> bool {
        std::iter::once(&self.chat)
            .chain(&self.parked)
            .any(|c| c.automation.as_deref() == Some(id) && c.running())
    }

    /// Starts each automation due at `now` that is not running already. Another window of the
    /// folder may have run one, or changed, stopped or deleted it: what the file says is read
    /// first.
    pub(crate) fn look_at_automations(&mut self, now: i64) -> Task<Msg> {
        if !self.automations_here().iter().any(|(_, a)| a.due(now)) {
            return Task::none();
        }
        self.read_others();
        let due: Vec<String> = self
            .automations_here()
            .into_iter()
            .filter(|(id, a)| a.due(now) && !self.automation_running(id))
            .map(|(id, _)| id.clone())
            .collect();
        Task::batch(due.iter().map(|id| self.run_automation(id, now)).collect::<Vec<_>>())
    }

    /// Starts automation `id` in a new conversation beside the one shown, its time written down
    /// first.
    fn run_automation(&mut self, id: &str, now: i64) -> Task<Msg> {
        let Some(automation) = self.prefs.more.automations.get(id).cloned() else {
            return Task::none();
        };
        let _ = self.set(|p| {
            if let Some(a) = p.more.automations.get_mut(id) {
                a.last = Some(now);
            }
        });
        let mut chat = self.new_chat();
        chat.mode = Some(automation.mode());
        chat.automation = Some(id.to_owned());
        let key = chat.key;
        self.parked.push(chat);
        self.toast = Some(fill(self.s.w().am_started, "name", &automation.name));
        self.start_run(key, automation.prompt, false)
    }

    /// The automations' screen's messages; any other comes back.
    pub(crate) fn update_automations(&mut self, msg: Msg) -> Result<Task<Msg>, Msg> {
        let Msg::Auto(msg) = msg else {
            return self.update_worktrees(msg);
        };
        let w = self.s.w();
        // What another window of the folder did to the automations, before this one acts on them.
        if matches!(
            msg,
            AutoMsg::Edit(_) | AutoMsg::Save | AutoMsg::Toggle(_) | AutoMsg::RunNow(_) | AutoMsg::Delete(_)
        ) {
            self.read_others();
        }
        let form = &mut self.auto_form;
        match msg {
            AutoMsg::Name(name) => form.name = name,
            AutoMsg::Prompt(action) => form.prompt.perform(action),
            AutoMsg::Kind(kind) => form.kind = kind,
            AutoMsg::Hours(hours) => form.hours = hours,
            AutoMsg::At(at) => form.at = at,
            AutoMsg::Mode(mode) => {
                if let Some(m) = MODES.iter().copied().find(|m| *m == mode) {
                    form.mode = m;
                }
            }
            AutoMsg::Clear => self.auto_form = Form::blank(),
            AutoMsg::Edit(id) => {
                let Some(a) = self.prefs.more.automations.get(&id) else {
                    return Ok(Task::none());
                };
                let (kind, hours, at) = match a.every {
                    Every::Hours { hours } => (Kind::Hours, hours.to_string(), "09:00".into()),
                    Every::Daily { at } => (Kind::Daily, "3".into(), show_at(at)),
                    Every::Weekdays { at } => (Kind::Weekdays, "3".into(), show_at(at)),
                };
                self.auto_form = Form {
                    editing: Some(id),
                    name: a.name.clone(),
                    prompt: text_editor::Content::with_text(&a.prompt),
                    kind,
                    hours,
                    at,
                    mode: a.mode(),
                    wrong: None,
                };
            }
            AutoMsg::Save => {
                let name = form.name.trim().to_owned();
                let prompt = form.prompt.text().trim().to_owned();
                if name.is_empty() || prompt.is_empty() {
                    form.wrong = Some(w.am_missing);
                    return Ok(Task::none());
                }
                let every = match form.kind {
                    Kind::Hours => match form.hours.trim().parse::<u32>() {
                        Ok(hours) if (1..=168).contains(&hours) => Every::Hours { hours },
                        _ => {
                            form.wrong = Some(w.am_bad_hours);
                            return Ok(Task::none());
                        }
                    },
                    Kind::Daily | Kind::Weekdays => {
                        let Some(at) = parse_at(&form.at) else {
                            form.wrong = Some(w.am_bad_at);
                            return Ok(Task::none());
                        };
                        if form.kind == Kind::Daily {
                            Every::Daily { at }
                        } else {
                            Every::Weekdays { at }
                        }
                    }
                };
                let (mode, editing) = (form.mode, form.editing.clone());
                let now = now_secs();
                let id = editing
                    .clone()
                    .unwrap_or_else(|| format!("a{}", Local::now().timestamp_millis()));
                let kept = self.prefs.more.automations.get(&id).cloned().unwrap_or_default();
                // Counted from now: a time changed to one passed today does not run at once.
                let automation = Automation {
                    name,
                    folder: self.folder_key(),
                    prompt,
                    every,
                    mode: mode.to_owned(),
                    on: kept.on || editing.is_none(),
                    since: now,
                    last: kept.last,
                };
                self.auto_form = Form::blank();
                return Ok(self.set(|p| {
                    p.more.automations.insert(id, automation);
                }));
            }
            AutoMsg::Toggle(id) => {
                let now = now_secs();
                return Ok(self.set(|p| {
                    if let Some(a) = p.more.automations.get_mut(&id) {
                        a.on = !a.on;
                        // Turned on, it waits for its next time: what was missed while off is not run.
                        a.since = now;
                    }
                }));
            }
            AutoMsg::RunNow(id) => {
                if self.automation_running(&id) {
                    return Ok(Task::none());
                }
                self.overlay = None;
                return Ok(self.run_automation(&id, now_secs()));
            }
            AutoMsg::Delete(id) => {
                if self.auto_form.editing.as_deref() == Some(id.as_str()) {
                    self.auto_form = Form::blank();
                }
                return Ok(self.set(|p| {
                    p.more.automations.remove(&id);
                }));
            }
            AutoMsg::Look => return Ok(self.look_at_automations(now_secs())),
        }
        Ok(Task::none())
    }

    /// Opens the automations' screen.
    pub(crate) fn show_automations(&mut self) -> Task<Msg> {
        if self.auto_form.mode.is_empty() {
            self.auto_form = Form::blank();
        }
        self.overlay = Some(Overlay::Automations);
        Task::none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn at(day: u32, h: u32, m: u32) -> NaiveDateTime {
        // 2026-11-16 is a Monday.
        NaiveDate::from_ymd_opt(2026, 11, day)
            .unwrap()
            .and_hms_opt(h, m, 0)
            .unwrap()
    }

    #[test]
    fn a_daily_time_is_due_once_it_has_come_and_once_only() {
        let every = Every::Daily { at: 9 * 60 };
        // Made at 8:00: not yet at 8:59, due at 9:00.
        assert!(!due(every, at(16, 8, 0), at(16, 8, 59)));
        assert!(due(every, at(16, 8, 0), at(16, 9, 0)));
        // Run at 9:00, it waits for tomorrow.
        assert!(!due(every, at(16, 9, 0), at(16, 23, 0)));
        assert_eq!(next(every, at(16, 9, 0), at(16, 23, 0)), at(17, 9, 0));
        // Made after its time, it waits for tomorrow too.
        assert!(!due(every, at(16, 10, 0), at(16, 12, 0)));
        // Missed for days, it runs once: the next look finds it run.
        assert!(due(every, at(10, 9, 0), at(16, 7, 0)));
        assert!(!due(every, at(16, 7, 0), at(16, 8, 0)));
    }

    #[test]
    fn weekdays_pass_over_the_weekend() {
        let every = Every::Weekdays { at: 8 * 60 + 30 };
        // Friday's run done, Saturday and Sunday are not due; Monday is.
        assert!(!due(every, at(20, 8, 30), at(21, 12, 0)));
        assert!(!due(every, at(20, 8, 30), at(22, 12, 0)));
        assert_eq!(next(every, at(20, 8, 30), at(21, 12, 0)), at(23, 8, 30));
        assert!(due(every, at(20, 8, 30), at(23, 8, 30)));
    }

    #[test]
    fn hours_are_counted_from_the_last_run() {
        let every = Every::Hours { hours: 3 };
        assert!(!due(every, at(16, 9, 0), at(16, 11, 59)));
        assert!(due(every, at(16, 9, 0), at(16, 12, 0)));
        assert_eq!(next(every, at(16, 9, 0), at(16, 10, 0)), at(16, 12, 0));
        // Never less than an hour.
        assert!(!due(Every::Hours { hours: 0 }, at(16, 9, 0), at(16, 9, 30)));
    }

    #[test]
    fn times_are_read_as_written() {
        assert_eq!(parse_at("09:00"), Some(540));
        assert_eq!(parse_at(" 7:05 "), Some(425));
        assert_eq!(parse_at("18：30"), Some(1110));
        assert_eq!(parse_at("24:00"), None);
        assert_eq!(parse_at("9"), None);
        assert_eq!(show_at(425), "07:05");
    }

    #[test]
    fn an_automation_turned_off_is_never_due_and_auto_is_no_mode_of_one() {
        let mut a = Automation {
            every: Every::Hours { hours: 1 },
            since: 0,
            ..Automation::default()
        };
        assert!(a.due(10 * 3600));
        a.on = false;
        assert!(!a.due(10 * 3600));
        a.mode = "auto".into();
        assert_eq!(a.mode(), "plan");
    }
}
