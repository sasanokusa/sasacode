//! The right panel: what the conversation changed, its plan, the commands it ran and what it used.

use iced::widget::{Column, Space, button, column, container, row, scrollable, text};
use iced::{Alignment, Length};

use iced::widget::tooltip;

use super::timeline::{command_copies, todo_list};
use super::{
    El, bold, clip, clip_start, diff_view, first_line, hline, mono, output_block, plus_minus, short_path, tag, tip,
};
use crate::app::{App, Msg, Tab};
use crate::helper::Draft;
use crate::i18n::{compact, fill};
use crate::icons::{Icon, icon};
use crate::theme::{RADIUS, RADIUS_L};
use crate::transcript::{TodoStatus, ToolState};

pub fn inspector(app: &App) -> El<'_> {
    let (t, s) = (app.t, app.s);
    let changes = app.chat.transcript.changes().count();
    let commands = app.chat.transcript.commands().count();
    let plan = app.chat.transcript.plan().map(|p| {
        format!(
            "{}/{}",
            p.iter().filter(|todo| todo.status == TodoStatus::Completed).count(),
            p.len()
        )
    });
    // Words only: with an icon each, the four do not fit the panel in English.
    let tab = |tab: Tab, key: u8, label: &'static str, count: Option<String>| -> El<'_> {
        let on = app.tab == tab;
        let mut content = row![
            text(label)
                .size(12.5)
                .color(if on { t.fg } else { t.fg_2 })
                .wrapping(text::Wrapping::None)
        ]
        .spacing(6)
        .align_y(Alignment::Center);
        if let Some(count) = count {
            content = content.push(
                text(count)
                    .size(11.5)
                    .font(mono())
                    .color(if on { t.accent } else { t.fg_3 }),
            );
        }
        tip(
            t,
            button(content)
                .padding([5, 8])
                .style(t.ghost(t.fg_2, on))
                .on_press(Msg::Tab(tab)),
            super::keys_for(crate::keys::Act::Tab(key)),
            tooltip::Position::Bottom,
        )
    };
    let tabs = row![
        tab(
            Tab::Changes,
            1,
            s.tab_changes,
            (changes > 0).then(|| changes.to_string())
        ),
        tab(Tab::Plan, 2, s.tab_plan, plan),
        tab(
            Tab::Commands,
            3,
            s.tab_cmd,
            (commands > 0).then(|| commands.to_string())
        ),
        tab(Tab::Usage, 4, s.tab_usage, None),
        tab(
            Tab::Git,
            5,
            s.tab_git,
            (!app.git.files.is_empty()).then(|| app.git.files.len().to_string())
        ),
        tab(Tab::Files, 6, s.w().tab_files, None),
    ]
    .spacing(1);
    // Where the panel is narrower than they are, they scroll sideways.
    let tabs = scrollable(tabs).direction(scrollable::Direction::Horizontal(
        scrollable::Scrollbar::new().width(0).scroller_width(0),
    ));

    let body = match app.tab {
        Tab::Changes => changes_tab(app),
        Tab::Plan => plan_tab(app),
        Tab::Commands => commands_tab(app),
        Tab::Usage => usage_tab(app),
        Tab::Git => git_tab(app),
        Tab::Files => files_tab(app),
    };
    container(
        column![
            container(tabs).padding([0, 8]).height(46).align_y(Alignment::Center),
            hline(t),
            scrollable(container(body).padding(12).width(Length::Fill))
                .id(crate::files::INSPECTOR)
                .direction(scrollable::Direction::Vertical(
                    scrollable::Scrollbar::new().width(8).scroller_width(8).margin(2)
                ))
                .style(t.scroll())
                .height(Length::Fill),
        ]
        .width(Length::Fill),
    )
    .width(app.insp_width())
    .height(Length::Fill)
    .style(t.fill(t.surface))
    .into()
}

/// What a tab says while it has nothing to show.
fn empty<'a>(app: &App, mark: Icon, head: &'a str, body: &'a str) -> El<'a> {
    let t = app.t;
    container(
        column![
            icon(mark, 22.0, t.line_2),
            text(head).size(13.5).font(bold()).color(t.fg_2),
            text(body).size(12.5).color(t.fg_3).align_x(Alignment::Center),
        ]
        .spacing(8)
        .align_x(Alignment::Center),
    )
    .center_x(Length::Fill)
    .padding([48, 20])
    .into()
}

/// How many half-width cells of monospaced text a card's head has room for, when `rest` of the
/// panel's width goes to everything else in it.
fn cells(app: &App, rest: f32) -> usize {
    ((app.insp_width() - rest) / 8.0).max(8.0) as usize
}

/// The name in a card's head: it takes the room the rest of the head leaves and is cut there, so
/// nothing is drawn over what follows it when the guess in [`cells`] was too generous.
fn name<'a>(label: iced::widget::Text<'a>) -> El<'a> {
    container(label.wrapping(text::Wrapping::None))
        .width(Length::Fill)
        .clip(true)
        .into()
}

/// A card with a head that folds its body away.
fn card<'a>(app: &'a App, id: &'a str, head: El<'a>, body: Option<El<'a>>) -> El<'a> {
    let t = app.t;
    let open = !app.chat.closed.contains(id);
    let mut col = column![
        button(
            row![
                head,
                icon(if open { Icon::ChevronDown } else { Icon::ChevronRight }, 11.0, t.fg_3)
            ]
            .spacing(8)
            .align_y(Alignment::Center)
        )
        .width(Length::Fill)
        .padding([7, 10])
        .style(t.ghost(t.fg, false))
        .on_press(Msg::Card(id.to_owned()))
    ];
    if let Some(body) = body.filter(|_| open) {
        col = col.push(container(body).padding(iced::Padding {
            top: 0.0,
            right: 8.0,
            bottom: 8.0,
            left: 8.0,
        }));
    }
    container(col)
        .width(Length::Fill)
        .style(t.boxed(t.raised, t.line, RADIUS_L))
        .into()
}

fn changes_tab(app: &App) -> El<'_> {
    let (t, s) = (app.t, app.s);
    let mut col = column![].spacing(10);
    if app.chat.transcript.changes().next().is_none() {
        return empty(app, Icon::Diff, s.ch_empty, s.ch_empty_p);
    }
    for (tool, change) in app.chat.transcript.changes() {
        let path = clip_start(&short_path(&change.path, &app.cwd), cells(app, 110.0));
        let mut head = row![
            icon(
                if change.created == Some(true) {
                    Icon::File
                } else {
                    Icon::Pencil
                },
                13.0,
                t.fg_3
            ),
            name(text(path).size(12.5).font(mono()).color(t.fg)),
        ]
        .spacing(8)
        .align_y(Alignment::Center);
        if change.created == Some(true) {
            head = head.push(tag(t, s.new_file, t.accent_wash, t.accent));
        }
        head = head.push(plus_minus(t, change.diff.added, change.diff.removed));
        let file = app.cwd.join(&change.path);
        if app.file_hover.as_deref() == Some(tool.id.as_str()) {
            head = head.push(file_marks(app, file.clone()));
        }
        col = col.push(on_file(
            file,
            &tool.id,
            card(app, &tool.id, head.into(), Some(diff_view(t, s, &change.diff, 200))),
        ));
    }
    col.into()
}

/// What git can do to a changed file as a whole: what of it is not staged, staged; what is,
/// unstaged; and all of it discarded, on a second press. Opened, viewed or shown in its folder from
/// its menu, as a right click opens it.
fn git_actions<'a>(app: &'a App, f: &'a crate::git::File) -> El<'a> {
    use crate::git::Op;
    let (t, s) = (app.t, app.s);
    let small = |label: &'a str, color: iced::Color, msg: Option<Msg>| {
        button(text(label).size(11.5).color(color).wrapping(text::Wrapping::None))
            .padding([2, 7])
            .style(t.ghost(color, false))
            .on_press_maybe(msg)
    };
    let busy = app.git_busy;
    let mut line = row![].spacing(2).align_y(Alignment::Center);
    if f.unstaged() || f.untracked() {
        line = line.push(small(
            s.stage,
            t.fg_2,
            (!busy).then(|| Msg::GitDo(Op::Stage(f.path.clone()))),
        ));
    }
    if f.staged() {
        line = line.push(small(
            s.unstage,
            t.fg_2,
            (!busy).then(|| Msg::GitDo(Op::Unstage(f.path.clone()))),
        ));
    }
    let sure = app.confirm_discard.as_ref() == Some(&f.path);
    line = line.push(small(
        if sure { s.discard_sure } else { s.discard },
        t.err,
        (!busy).then(|| Msg::GitDo(Op::Discard(f.path.clone()))),
    ));
    line.into()
}

/// A changed file viewed, opened in an editor or shown in its folder: marks in its head while the
/// pointer is on it, each named by its tooltip.
fn file_marks<'a>(app: &'a App, file: std::path::PathBuf) -> El<'a> {
    let (t, s) = (app.t, app.s);
    let mark = |which: Icon, hint: &'a str, msg: Msg| {
        tip(
            t,
            button(icon(which, 12.0, t.fg_2))
                .padding([3, 5])
                .style(t.ghost(t.fg_2, false))
                .on_press(msg),
            hint.to_owned(),
            tooltip::Position::Bottom,
        )
    };
    let mut line = row![].spacing(0).align_y(Alignment::Center);
    if file.is_file() {
        line = line.push(mark(Icon::File, s.w().view, Msg::ViewFile(file.clone(), None)));
    }
    if file.exists() {
        line = line
            .push(mark(Icon::Pencil, s.open_editor, Msg::Editor(file.clone())))
            .push(mark(Icon::Folder, s.reveal, Msg::Reveal(file)));
    }
    line.into()
}

/// The folder's repository: its branch, what is not committed with each file's diff, and the way to
/// commit, push and open a pull request.
fn git_tab(app: &App) -> El<'_> {
    use crate::git::Op;
    let (t, s) = (app.t, app.s);
    let g = &app.git;
    let w = s.w();
    if !g.repo {
        return column![
            empty(app, Icon::Branch, s.tab_git, s.git_none),
            container(
                button(text(w.git_init).size(12.5))
                    .padding([5, 12])
                    .style(t.outline(t.fg))
                    .on_press_maybe((!app.git_busy).then_some(Msg::GitDo(Op::Init)))
            )
            .center_x(Length::Fill),
        ]
        .spacing(8)
        .into();
    }
    let mut head = row![icon(Icon::Branch, 14.0, t.fg_3)]
        .spacing(8)
        .align_y(Alignment::Center);
    // The branch: a click lists the others to switch to, or makes a new one.
    head = head.push(tip(
        t,
        button(
            row![
                text(g.branch.as_deref().unwrap_or("HEAD"))
                    .size(12.5)
                    .font(mono())
                    .color(t.fg),
                icon(Icon::ChevronDown, 10.0, t.fg_3),
            ]
            .spacing(4)
            .align_y(Alignment::Center),
        )
        .padding([2, 6])
        .style(t.ghost(t.fg, false))
        .on_press(Msg::Show(Some(crate::overlay::Overlay::picker(
            crate::overlay::Pick::Branches,
        )))),
        w.branch_switch.to_owned(),
        tooltip::Position::Bottom,
    ));
    if g.ahead > 0 {
        head = head.push(tag(t, fill(s.ahead, "n", g.ahead), t.accent_wash, t.accent));
    }
    if g.behind > 0 {
        head = head.push(tag(t, fill(w.behind, "n", g.behind), t.inset, t.fg_2));
    }
    head = head.push(Space::new().width(Length::Fill));
    if app.git_busy {
        head = head.push(text(s.git_busy).size(12).color(t.fg_3));
    }
    // The agent asked to review what is not committed.
    if !g.files.is_empty() {
        head = head.push(
            button(text(w.review_ask).size(12).color(t.fg_2).wrapping(text::Wrapping::None))
                .padding([3, 8])
                .style(t.ghost(t.fg_2, false))
                .on_press(Msg::AskReview),
        );
    }
    head = head.push(tip(
        t,
        button(icon(Icon::Refresh, 13.0, t.fg_2))
            .padding(5)
            .style(t.ghost(t.fg_2, false))
            .on_press(Msg::GitRefresh),
        s.git_refresh.to_owned(),
        tooltip::Position::Bottom,
    ));
    let mut col = column![head].spacing(10);

    let busy = app.git_busy;
    let drafting = app.drafting.contains(&Draft::Commit);
    // The message, which the model can write: the mark at its end.
    let draft = tip(
        t,
        button(icon(Icon::Spark, 14.0, if drafting { t.accent } else { t.fg_2 }))
            .padding([6, 7])
            .style(t.ghost(t.fg_2, false))
            .on_press_maybe((!busy && !drafting && !g.files.is_empty()).then_some(Msg::Draft(Draft::Commit))),
        format!(
            "{}\n{}",
            if drafting { w.drafting } else { w.draft_commit },
            w.draft_commit_hint
        ),
        tooltip::Position::Bottom,
    );
    let message = row![
        iced::widget::text_input(s.commit_ph, &app.commit_msg)
            .on_input(Msg::CommitText)
            .on_submit(Msg::GitDo(Op::Commit(app.commit_msg.clone())))
            .size(12.5)
            .padding([6, 8])
            .style(t.input()),
        draft,
    ]
    .spacing(4)
    .align_y(Alignment::Center);
    // Only what can be done here: no pull without an upstream, no new pull request beside one.
    let mut buttons = row![
        tip(
            t,
            button(text(s.commit).size(12.5))
                .padding([5, 12])
                .style(t.primary())
                .on_press_maybe(
                    (!busy && !g.files.is_empty() && !app.commit_msg.trim().is_empty())
                        .then(|| Msg::GitDo(Op::Commit(app.commit_msg.clone())))
                ),
            s.commit_hint.to_owned(),
            tooltip::Position::Bottom,
        ),
        button(text(s.push).size(12.5))
            .padding([5, 12])
            .style(t.outline(t.fg))
            .on_press_maybe((!busy && g.branch.is_some()).then_some(Msg::GitDo(Op::Push))),
    ]
    .spacing(6)
    .align_y(Alignment::Center);
    if g.upstream {
        buttons = buttons.push(
            button(text(w.pull).size(12.5))
                .padding([5, 12])
                .style(t.outline(t.fg))
                .on_press_maybe((!busy).then_some(Msg::GitDo(Op::Pull))),
        );
    }
    if app.pr.is_none() && !app.pr_loading {
        buttons = buttons.push(
            button(text(s.pr).size(12.5))
                .padding([5, 12])
                .style(t.outline(t.fg))
                .on_press_maybe((!busy && g.branch.is_some()).then_some(Msg::PrOpen)),
        );
    }
    let commit = column![message, buttons.wrap()].spacing(6);
    col = col.push(commit);
    if let Some(pr) = pr_part(app) {
        col = col.push(pr);
    }
    col = col.push(scope_bar(app));
    if app.git_scope != crate::scope::Scope::Changes {
        return col.push(scoped_files(app)).into();
    }

    if g.files.is_empty() {
        return col.push(empty(app, Icon::Check, s.git_clean, "")).into();
    }
    if !app.chat.review.is_empty() {
        col = col.push(review_found(app));
    }
    // By folder: each folder's changed files under its line, which folds them.
    let folder_of = |path: &str| {
        path.rsplit_once('/')
            .map_or(String::new(), |(dir, _)| format!("{dir}/"))
    };
    let mut files: Vec<&crate::git::File> = g.files.iter().collect();
    files.sort_by(|a, b| (folder_of(&a.path), &a.path).cmp(&(folder_of(&b.path), &b.path)));
    let mut last: Option<String> = None;
    for f in files {
        let folder = folder_of(&f.path);
        if last.as_ref() != Some(&folder) {
            let count = g.files.iter().filter(|x| folder_of(&x.path) == folder).count();
            let shut = app.git_folded.contains(&folder);
            let label = if folder.is_empty() {
                w.git_top.to_owned()
            } else {
                folder.clone()
            };
            col = col.push(
                button(
                    row![
                        icon(if shut { Icon::ChevronRight } else { Icon::ChevronDown }, 11.0, t.fg_3),
                        icon(Icon::Folder, 12.0, t.fg_3),
                        text(clip_start(&label, cells(app, 120.0)))
                            .size(12)
                            .font(mono())
                            .color(t.fg_2)
                            .wrapping(text::Wrapping::None),
                        text(format!("({count})")).size(11).color(t.fg_3),
                    ]
                    .spacing(6)
                    .align_y(Alignment::Center),
                )
                .width(Length::Fill)
                .padding([3, 6])
                .style(t.ghost(t.fg_2, false))
                .on_press(Msg::GitFold(folder.clone())),
            );
            last = Some(folder.clone());
        }
        if app.git_folded.contains(&folder) {
            continue;
        }
        let letters = format!("{}{}", f.index, f.tree).replace(' ', "·");
        let shown = f.path.strip_prefix(&folder).unwrap_or(&f.path);
        let path = clip_start(shown, cells(app, 130.0));
        let mut head = row![
            text(letters)
                .size(11.5)
                .font(mono())
                .color(if f.staged() { t.accent } else { t.fg_3 }),
            name(text(path).size(12.5).font(mono()).color(t.fg)),
        ]
        .spacing(8)
        .align_y(Alignment::Center);
        if f.untracked() {
            head = head.push(tag(t, s.new_file, t.accent_wash, t.accent));
        }
        let (added, removed) = f.counts();
        head = head.push(plus_minus(t, added, removed));
        let file = g.top.join(&f.path);
        if app.file_hover.as_deref() == Some(f.path.as_str()) || app.confirm_discard.as_ref() == Some(&f.path) {
            head = head.push(git_actions(app, f));
        }
        let mut body = column![].spacing(6);
        // What is staged and what is not, apart: each hunk is staged, unstaged or thrown away
        // by itself. A file git does not know yet is staged whole.
        let both = !f.staged_diff.is_empty() && !f.unstaged_diff.is_empty();
        if !f.staged_diff.is_empty() {
            if both {
                body = body.push(text(w.staged_head).size(11.5).color(t.fg_3));
            }
            body = body.push(hunks(app, f, true));
        }
        if !f.unstaged_diff.is_empty() {
            if both {
                body = body.push(text(w.unstaged_head).size(11.5).color(t.fg_3));
            }
            body = body.push(hunks(app, f, false));
        }
        col = col.push(on_file(
            file,
            &f.path,
            card(app, &f.path, head.into(), Some(body.into())),
        ));
    }
    col.into()
}

/// What a right click on a file of the folder opens: its menu.
fn menu_on(file: std::path::PathBuf, content: El<'_>) -> El<'_> {
    iced::widget::mouse_area(content)
        .on_right_press(Msg::OpenMenu(crate::rows::MenuFor::File(file)))
        .into()
}

/// A changed file's card, under the pointer or not: a right click opens the file's menu, and
/// the pointer on it shows what can be done with it in its head.
fn on_file<'a>(file: std::path::PathBuf, key: &str, content: El<'a>) -> El<'a> {
    iced::widget::mouse_area(content)
        .on_right_press(Msg::OpenMenu(crate::rows::MenuFor::File(file)))
        .on_enter(Msg::FileHover(key.to_owned(), true))
        .on_exit(Msg::FileHover(key.to_owned(), false))
        .into()
}

/// The branch's pull request: its state, its checks, what its reviewers said, and what can be done
/// about them. Nothing while it is not known whether there is one.
fn pr_part(app: &App) -> Option<El<'_>> {
    use crate::git::{CheckState, Op};
    let (t, s) = (app.t, app.s);
    let w = s.w();
    let Some(pr) = &app.pr else {
        return app
            .pr_loading
            .then(|| text(w.pr_loading).size(11.5).color(t.fg_3).into());
    };
    let small = |label: &'static str, color: iced::Color, msg: Option<Msg>| {
        button(text(label).size(11.5).color(color).wrapping(text::Wrapping::None))
            .padding([3, 8])
            .style(t.ghost(color, false))
            .on_press_maybe(msg)
    };
    let state = match pr.state.as_str() {
        "MERGED" => tag(t, app.pr_state(pr), t.accent_wash, t.accent),
        "CLOSED" => tag(t, app.pr_state(pr), t.inset, t.fg_2),
        _ => tag(t, app.pr_state(pr), t.add_wash, t.add),
    };
    let mut head = row![
        text(format!("#{}", pr.number)).size(12.5).font(mono()).color(t.fg_2),
        name(text(pr.title.as_str()).size(12.5).font(bold()).color(t.fg)),
        state,
    ]
    .spacing(8)
    .align_y(Alignment::Center);
    let decision = match pr.decision.as_str() {
        "APPROVED" => Some((w.approved, t.add)),
        "CHANGES_REQUESTED" => Some((w.changes_requested, t.err)),
        "REVIEW_REQUIRED" => Some((w.review_required, t.fg_3)),
        _ => None,
    };
    if let Some((label, color)) = decision {
        head = head.push(text(label).size(11.5).color(color).wrapping(text::Wrapping::None));
    }
    if pr.draft {
        head = head.push(tag(t, w.draft_tag, t.inset, t.fg_2));
    }
    if pr.auto_merge {
        head = head.push(tag(t, w.auto_merge_on, t.accent_wash, t.accent));
    }
    let mut col = column![head].spacing(6);

    if !pr.checks.is_empty() {
        let mut checks = column![text(w.checks).size(11.5).color(t.fg_3)].spacing(2);
        for c in &pr.checks {
            let (mark, color, said) = match c.state {
                CheckState::Passed => (Icon::Check, t.add, w.check_passed),
                CheckState::Failed => (Icon::Close, t.err, w.check_failed),
                CheckState::Pending => (Icon::Dot, t.warn, w.check_pending),
                CheckState::Skipped => (Icon::Dot, t.fg_3, w.check_skipped),
            };
            let line = row![
                icon(mark, 12.0, color),
                name(text(c.name.as_str()).size(12).font(mono()).color(t.fg)),
                text(said).size(11.5).color(color).wrapping(text::Wrapping::None),
            ]
            .spacing(6)
            .align_y(Alignment::Center);
            checks = checks.push(if c.url.is_empty() {
                El::from(line)
            } else {
                iced::widget::mouse_area(line)
                    .interaction(iced::mouse::Interaction::Pointer)
                    .on_press(Msg::Link(c.url.clone()))
                    .into()
            });
        }
        col = col.push(checks);
    }

    let busy = app.git_busy;
    let mut actions = row![small(w.open_browser, t.fg_2, Some(Msg::Link(pr.url.clone())))]
        .spacing(2)
        .align_y(Alignment::Center);
    if pr.failing().next().is_some() {
        actions = actions.push(small(w.fix_checks, t.err, Some(Msg::FixChecks)));
    }
    let how = app.prefs.more.merge_method.clone();
    if pr.state == "OPEN" {
        actions = actions.push(small(
            if app.confirm_merge { w.merge_sure } else { w.merge },
            t.accent,
            (!busy && !pr.draft).then(|| Msg::GitDo(Op::Merge(how.clone()))),
        ));
        if !pr.auto_merge && !pr.draft {
            actions = actions.push(small(
                w.auto_merge,
                t.fg_2,
                (!busy).then(|| Msg::GitDo(Op::AutoMerge(how.clone()))),
            ));
        }
        actions = actions.push(small(
            if pr.draft { w.ready } else { w.to_draft },
            t.fg_2,
            (!busy).then_some(Msg::GitDo(Op::Ready(pr.draft))),
        ));
        actions = actions.push(small(
            w.update_branch,
            t.fg_2,
            (!busy).then_some(Msg::GitDo(Op::UpdateBranch)),
        ));
    }
    actions = actions.push(small(
        s.git_refresh,
        t.fg_2,
        (!app.pr_loading).then_some(Msg::PrRefresh),
    ));
    col = col.push(actions.wrap());
    if pr.state == "OPEN" {
        let mut ways = row![text(w.merge_how).size(11.5).color(t.fg_3)]
            .spacing(4)
            .align_y(Alignment::Center);
        for way in ["merge", "squash", "rebase"] {
            let on = method_of(&app.prefs.more.merge_method) == way;
            let b = button(text(way).size(11).font(mono()))
                .padding([2, 6])
                .on_press(Msg::SetMergeMethod(way));
            ways = ways.push(if on {
                b.style(t.primary())
            } else {
                b.style(t.ghost(t.fg_2, false))
            });
        }
        col = col.push(ways);
    }

    if !pr.comments.is_empty() {
        let mut said = column![text(w.reviews).size(11.5).color(t.fg_3)].spacing(6);
        for (i, remark) in pr.comments.iter().enumerate() {
            let mut who = row![text(remark.author.as_str()).size(12).font(bold()).color(t.fg)]
                .spacing(6)
                .align_y(Alignment::Center);
            if let (Some(path), Some(line)) = (&remark.path, remark.line) {
                who = who.push(
                    text(clip_start(&format!("{path}:{line}"), cells(app, 190.0)))
                        .size(11.5)
                        .font(mono())
                        .color(t.fg_3),
                );
            }
            who = who.push(Space::new().width(Length::Fill));
            who = who.push(small(w.address, t.fg_2, Some(Msg::Address(i))));
            said = said.push(
                container(
                    column![
                        who,
                        text(clip(remark.body.trim(), 600))
                            .size(12)
                            .color(t.fg_2)
                            .wrapping(text::Wrapping::WordOrGlyph),
                    ]
                    .spacing(2),
                )
                .width(Length::Fill)
                .padding([6, 8])
                .style(t.boxed(t.raised, t.line, RADIUS)),
            );
        }
        col = col.push(said);
    }
    Some(
        container(col)
            .width(Length::Fill)
            .padding([8, 10])
            .style(t.boxed(t.raised, t.line, RADIUS_L))
            .into(),
    )
}

/// The merge method kept, as gh names it.
fn method_of(kept: &str) -> &str {
    match kept {
        "squash" | "rebase" => kept,
        _ => "merge",
    }
}

/// What the git tab compares, and how its diffs are drawn.
fn scope_bar(app: &App) -> El<'_> {
    use crate::scope::Scope;
    let t = app.t;
    let w = app.s.w();
    let pick = |label: String, on: bool, msg: Msg| -> El<'_> {
        let b = button(text(label).size(11.5).wrapping(text::Wrapping::None))
            .padding([3, 8])
            .on_press(msg);
        if on {
            b.style(t.primary()).into()
        } else {
            b.style(t.outline(t.fg_2)).into()
        }
    };
    let commit = match &app.git_scope {
        Scope::Commit(sha) => app
            .scope_commits
            .iter()
            .find(|(h, _)| h == sha)
            .map(|(h, subject)| format!("{h} {}", clip(subject, 28))),
        _ => None,
    };
    let choices: Vec<String> = app
        .scope_commits
        .iter()
        .map(|(h, subject)| format!("{h} {}", clip(subject, 40)))
        .collect();
    let scopes = row![
        pick(
            w.scope_changes.to_owned(),
            app.git_scope == Scope::Changes,
            Msg::GitScope(Scope::Changes)
        ),
        pick(
            w.scope_branch.to_owned(),
            app.git_scope == Scope::Branch,
            Msg::GitScope(Scope::Branch)
        ),
        pick(
            w.scope_turn.to_owned(),
            app.git_scope == Scope::Turn,
            Msg::GitScope(Scope::Turn)
        ),
        iced::widget::pick_list(choices, commit, |picked: String| {
            Msg::GitScope(Scope::Commit(picked.split(' ').next().unwrap_or_default().to_owned()))
        })
        .placeholder(w.scope_commit)
        .text_size(11.5)
        .padding([3, 6])
        .style(t.pick(t.fg_2))
        .menu_style(t.menu()),
    ]
    .spacing(6)
    .align_y(Alignment::Center);
    // How the diffs are drawn: in a menu, each way marked while it is on.
    let more = &app.prefs.more;
    let changed = more.split_diff || more.hide_whitespace || more.diff_nowrap;
    let view = button(
        row![
            icon(Icon::Sliders, 12.0, if changed { t.accent } else { t.fg_2 }),
            text(w.diff_view)
                .size(11.5)
                .color(t.fg_2)
                .wrapping(text::Wrapping::None),
            icon(Icon::ChevronDown, 10.0, t.fg_3),
        ]
        .spacing(5)
        .align_y(Alignment::Center),
    )
    .padding([3, 8])
    .style(t.ghost(t.fg_2, false))
    .on_press(Msg::OpenMenu(crate::rows::MenuFor::DiffView));
    row![container(scopes.wrap()).width(Length::Fill), view]
        .spacing(6)
        .align_y(Alignment::Center)
        .into()
}

/// The files of the branch or of a commit: by folder, each folded by its head, marked as viewed.
fn scoped_files(app: &App) -> El<'_> {
    let (t, s) = (app.t, app.s);
    let w = s.w();
    if app.scope_loading && app.scoped.is_none() {
        return text(w.scope_loading).size(12).color(t.fg_3).into();
    }
    let scoped = match &app.scoped {
        Some(Ok(scoped)) => scoped,
        Some(Err(e)) => return text(e.as_str()).size(12).color(t.err).into(),
        None => return Space::new().into(),
    };
    let mut col = column![].spacing(8);
    let keys: Vec<String> = scoped
        .files
        .iter()
        .map(|(p, d)| crate::scope::viewed_key(p, d))
        .collect();
    let seen = keys.iter().filter(|k| app.viewed(k)).count();
    col = col.push(
        row![
            text(if app.git_scope == crate::scope::Scope::Turn {
                w.scope_turn_base.to_owned()
            } else {
                fill(w.scope_base, "base", &scoped.base)
            })
            .size(11.5)
            .color(t.fg_3),
            Space::new().width(Length::Fill),
            text(fill(&fill(w.viewed_n, "n", seen), "all", keys.len()))
                .size(11.5)
                .color(t.fg_3),
        ]
        .align_y(Alignment::Center),
    );
    if scoped.files.is_empty() {
        return col.push(empty(app, Icon::Check, w.scope_empty, "")).into();
    }
    let mut folder = None;
    for ((path, diff), key) in scoped.files.iter().zip(keys) {
        // A folder's name once, over its files.
        let here = path.rsplit_once('/').map(|(d, _)| d);
        if here != folder {
            folder = here;
            if let Some(dir) = here {
                col = col.push(text(format!("{dir}/")).size(11.5).font(mono()).color(t.fg_3));
            }
        }
        let viewed = app.viewed(&key);
        let file_name = path.rsplit_once('/').map_or(path.as_str(), |(_, f)| f);
        let head = row![
            iced::widget::checkbox(viewed)
                .label(w.viewed)
                .on_toggle(move |_| Msg::Viewed(key.clone()))
                .size(13)
                .text_size(11),
            name(
                text(clip_start(file_name, cells(app, 200.0)))
                    .size(12.5)
                    .font(mono())
                    .color(if viewed { t.fg_3 } else { t.fg })
            ),
            plus_minus(t, diff.added, diff.removed),
        ]
        .spacing(8)
        .align_y(Alignment::Center);
        let body: El<'_> = if viewed {
            Space::new().into()
        } else if app.prefs.more.split_diff {
            split_view(app, diff)
        } else {
            let wide = unwrapped(app, diff.lines.iter().take(400).map(|l| l.text.as_str()), 1.0);
            super::diff_view_in(t, s, diff, 400, wide)
        };
        col = col.push(on_file(
            app.git.top.join(path),
            path,
            card(app, path, head.into(), (!viewed).then_some(body)),
        ));
    }
    col.into()
}

/// A diff in two columns: before on the left, after on the right.
fn split_view<'a>(app: &'a App, diff: &'a crate::diff::Diff) -> El<'a> {
    use crate::diff::Sign;
    let t = app.t;
    let wide = unwrapped(app, diff.lines.iter().take(800).map(|l| l.text.as_str()), 0.5);
    let (_, wrapping) = super::diff_line_width(wide);
    let width = wide.map_or(Length::FillPortion(1), Length::Fixed);
    let side = move |line: Option<&'a crate::diff::Line>, left: bool| -> El<'a> {
        let Some(line) = line else {
            return container(Space::new()).width(width).into();
        };
        let fill = match line.sign {
            Sign::Del if left => Some(t.del_wash),
            Sign::Add if !left => Some(t.add_wash),
            _ => None,
        };
        container(
            text(line.text.as_str())
                .size(11.5)
                .font(mono())
                .color(if line.sign == Sign::Hunk { t.fg_3 } else { t.fg })
                .wrapping(wrapping),
        )
        .width(width)
        .padding([1, 6])
        .style(move |_| container::Style {
            background: fill.map(Into::into),
            ..container::Style::default()
        })
        .into()
    };
    let mut lines = column![];
    for (left, right) in crate::scope::side_by_side(diff).into_iter().take(400) {
        lines = lines.push(row![side(left, true), side(right, false)].spacing(2));
    }
    super::diff_box(t, lines, wide)
}

/// The hunks of a file's staged diff, or of what is not staged: each with what can be done to it
/// alone, its lines taking a comment when clicked.
fn hunks<'a>(app: &'a App, f: &'a crate::git::File, staged: bool) -> El<'a> {
    use crate::diff::Sign;
    use crate::git::{HunkAct, Op};
    let (t, s) = (app.t, app.s);
    let diff = if staged { &f.staged_diff } else { &f.unstaged_diff };
    let path = app.git.top.join(&f.path);
    let numbers = crate::git::numbers(diff);
    let small = |label: &'a str, color: iced::Color, msg: Option<Msg>| {
        button(text(label).size(11).color(color).wrapping(text::Wrapping::None))
            .padding([1, 6])
            .style(t.ghost(color, false))
            .on_press_maybe(msg)
    };
    let busy = app.git_busy;
    let wide = unwrapped(app, diff.lines.iter().take(300).map(|l| l.text.as_str()), 1.0);
    let (width, wrapping) = super::diff_line_width(wide);
    // Comments under a line stay as wide as the panel when the lines scroll sideways.
    let note_width = wide.map_or(Length::Fill, |_| Length::Fixed(app.insp_width() - 48.0));
    // The review's findings on this file, looked up once.
    let found: Vec<(usize, &crate::review::Comment)> = app
        .chat
        .review
        .iter()
        .enumerate()
        .filter(|(_, c)| crate::review::same_file(&c.path, &path))
        .collect();
    let mut lines = column![];
    let mut hunk = 0usize;
    for (line, n) in diff.lines.iter().zip(numbers).take(300) {
        if line.sign == Sign::Hunk {
            let at = hunk;
            hunk += 1;
            let op = |act| {
                Msg::GitDo(Op::Hunk {
                    path: f.path.clone(),
                    act,
                    at,
                    head: line.text.clone(),
                })
            };
            let mut bar = row![
                text(line.text.as_str())
                    .size(11.5)
                    .font(mono())
                    .color(t.fg_3)
                    .width(Length::Fill)
                    .wrapping(text::Wrapping::None),
            ]
            .spacing(2)
            .align_y(Alignment::Center);
            if f.untracked() {
                // A new file is staged whole, above.
            } else if staged {
                bar = bar.push(small(s.unstage, t.fg_2, (!busy).then(|| op(HunkAct::Unstage))));
            } else {
                let sure = app.confirm_discard.as_deref() == Some(format!("{}#{at}", f.path).as_str());
                bar = bar
                    .push(small(s.stage, t.fg_2, (!busy).then(|| op(HunkAct::Stage))))
                    .push(small(
                        if sure { s.discard_sure } else { s.discard },
                        t.err,
                        (!busy).then(|| op(HunkAct::Discard)),
                    ));
            }
            lines = lines.push(
                container(bar.clip(true))
                    .width(width)
                    .padding([2, 6])
                    .style(t.fill(t.inset)),
            );
            continue;
        }
        let (mark, color, wash) = match line.sign {
            Sign::Add => ("+", t.add, Some(t.add_wash)),
            Sign::Del => ("−", t.del, Some(t.del_wash)),
            _ => (" ", t.fg_2, None),
        };
        let shown = container(
            row![
                text(n.map(|n| n.to_string()).unwrap_or_default())
                    .size(11)
                    .font(mono())
                    .color(t.fg_3)
                    .width(34),
                text(mark).size(12).font(mono()).color(color).width(12),
                text(line.text.as_str())
                    .size(12)
                    .font(mono())
                    .color(t.fg)
                    .wrapping(wrapping),
            ]
            .spacing(2),
        )
        .width(width)
        .padding([1, 6])
        .style(move |_| container::Style {
            background: wash.map(Into::into),
            ..container::Style::default()
        });
        let Some(n) = n else {
            lines = lines.push(shown);
            continue;
        };
        lines = lines.push(
            iced::widget::mouse_area(shown)
                .interaction(iced::mouse::Interaction::Pointer)
                .on_press(Msg::CommentAt(path.clone(), n, staged)),
        );
        // What was said about this line, what the agent's review found on it, and the comment
        // being written under it.
        for (i, c) in app.chat.comments.iter().enumerate() {
            if c.path == path && c.line == n {
                lines = lines.push(container(comment_note(app, i, c)).width(note_width));
            }
        }
        for (i, c) in &found {
            if c.line == n {
                lines = lines.push(container(review_note(app, *i, c, false)).width(note_width));
            }
        }
        if let Some(draft) = &app.commenting
            && draft.path == path
            && draft.line == n
            && draft.staged == staged
        {
            lines = lines.push(container(draft_box(app, draft)).width(note_width));
        }
    }
    super::diff_box(t, lines, wide)
}

/// How wide the git tab draws diff lines when it keeps each whole, or none when it wraps them.
/// `share` is the part of the panel's width one column of lines takes.
fn unwrapped<'a>(app: &App, lines: impl Iterator<Item = &'a str>, share: f32) -> Option<f32> {
    app.prefs
        .more
        .diff_nowrap
        .then(|| super::unwrapped_width(lines, (app.insp_width() - 48.0) * share))
}

/// The folder's scripts run from the window: each one's state, its output as it comes, and the
/// way to stop it, run it again, or show it to the agent.
fn script_runs(app: &App) -> El<'_> {
    let (t, s) = (app.t, app.s);
    let w = s.w();
    let mut col = column![text(w.script_runs).size(12).color(t.fg_3)].spacing(8);
    for run in &app.script_runs {
        let state = match run.ended {
            None => tag(t, w.script_running, t.accent_wash, t.accent),
            Some(Some(0)) => tag(t, fill(w.script_exit, "code", 0), t.inset, t.fg_2),
            Some(Some(code)) => tag(t, fill(w.script_exit, "code", code), t.err_wash, t.err),
            Some(None) => tag(t, s.killed_abort, t.inset, t.fg_2),
        };
        let small = |label: &'static str, color: iced::Color, msg: Msg| {
            button(text(label).size(11.5).color(color).wrapping(text::Wrapping::None))
                .padding([3, 8])
                .style(t.ghost(color, false))
                .on_press(msg)
        };
        let label = if run.name.trim().is_empty() {
            run.command.as_str()
        } else {
            run.name.as_str()
        };
        let mut head = row![
            icon(Icon::Play, 12.0, t.fg_3),
            name(text(clip(label, cells(app, 200.0))).size(12.5).font(mono()).color(t.fg)),
            state,
        ]
        .spacing(8)
        .align_y(Alignment::Center);
        if let Some(d) = run.took.filter(|d| d.as_millis() >= 1000) {
            head = head.push(
                text(s.duration(d))
                    .size(11.5)
                    .color(t.fg_3)
                    .wrapping(text::Wrapping::None),
            );
        }
        let mut actions = row![].spacing(2);
        if run.running() {
            actions = actions.push(small(w.stop, t.err, Msg::StopScript(run.id)));
        } else if let Some(i) = app.scripts().iter().position(|sc| sc.command == run.command) {
            actions = actions.push(small(w.script_again, t.fg_2, Msg::RunScript(i)));
        }
        if !run.output.trim().is_empty() {
            actions = actions.push(small(w.script_to_agent, t.fg_2, Msg::ScriptToAgent(run.id)));
        }
        col = col.push(
            container(column![head, output_block(t, s, &run.output, 40), actions.wrap()].spacing(6))
                .width(Length::Fill)
                .padding([8, 10])
                .style(t.boxed(t.raised, t.line, RADIUS_L)),
        );
    }
    col.into()
}

/// The comment being written under a line.
fn draft_box<'a>(app: &'a App, draft: &'a crate::review::Draft) -> El<'a> {
    let t = app.t;
    let w = app.s.w();
    let small = |label: &'a str, msg: Msg| {
        button(text(label).size(11).color(t.fg_2).wrapping(text::Wrapping::None))
            .padding([1, 6])
            .style(t.ghost(t.fg_2, false))
            .on_press(msg)
    };
    container(
        column![
            iced::widget::text_input(w.comment_ph, &draft.text)
                .id(crate::review::COMMENT)
                .on_input(Msg::CommentText)
                .on_submit(Msg::CommentSave)
                .size(12.5)
                .padding([5, 8])
                .style(t.input()),
            row![
                button(text(w.comment_add).size(12))
                    .padding([3, 10])
                    .style(t.primary())
                    .on_press_maybe((!draft.text.trim().is_empty()).then_some(Msg::CommentSave)),
                small(w.comment_quote, Msg::CommentQuote),
                small(w.cancel, Msg::CommentCancel),
            ]
            .spacing(4)
            .align_y(Alignment::Center)
            .wrap(),
        ]
        .spacing(4),
    )
    .padding([6, 8])
    .style(t.fill(t.surface))
    .into()
}

/// The files tab: the project's files found by what is typed, or the one being read, its lines
/// colored by its syntax and each one commented on by a click.
fn files_tab(app: &App) -> El<'_> {
    let (t, s) = (app.t, app.s);
    let w = s.w();
    let Some(v) = &app.viewing else {
        let mode = |label: &'static str, on: bool, msg: Msg| -> El<'_> {
            let b = button(text(label).size(12).wrapping(text::Wrapping::None))
                .padding([3, 10])
                .on_press(msg);
            if on {
                b.style(t.primary()).into()
            } else {
                b.style(t.ghost(t.fg_2, false)).into()
            }
        };
        let modes = row![
            mode(w.grep_names, !app.file_grep, Msg::FileGrepMode(false)),
            mode(w.grep_text, app.file_grep, Msg::FileGrepMode(true)),
        ]
        .spacing(4);
        let field = iced::widget::text_input(if app.file_grep { w.grep_ph } else { w.files_ph }, &app.file_query)
            .id(crate::files::FILES_FIELD)
            .on_input(Msg::FileQuery)
            .on_submit_maybe(app.file_grep.then_some(Msg::FileGrep))
            .size(13)
            .padding([6, 10])
            .style(t.input());
        if app.file_grep {
            return column![modes, field, grep_list(app)].spacing(10).into();
        }
        let mut list = column![].spacing(1);
        if app.files.is_empty() {
            list = list.push(text(w.files_listing).size(12.5).color(t.fg_3));
        }
        // Nothing typed: the project as a tree, opened folder by folder.
        if app.file_query.trim().is_empty() && !app.files.is_empty() {
            for r in crate::files::tree(&app.files, &app.tree_open)
                .into_iter()
                .take(crate::files::LISTED)
            {
                let mark = if !r.folder {
                    Icon::File
                } else if r.open {
                    Icon::ChevronDown
                } else {
                    Icon::ChevronRight
                };
                let line = row![
                    Space::new().width(14.0 * r.depth as f32),
                    icon(mark, 11.0, t.fg_3),
                    text(r.name)
                        .size(12)
                        .font(if r.folder { bold() } else { mono() })
                        .color(if r.folder { t.fg_2 } else { t.fg })
                        .wrapping(text::Wrapping::None),
                ]
                .spacing(6)
                .align_y(Alignment::Center);
                let msg = if r.folder {
                    Msg::TreeFold(r.path.clone())
                } else {
                    Msg::ViewFile(app.cwd.join(&r.path), None)
                };
                let b = button(line)
                    .width(Length::Fill)
                    .padding([3, 8])
                    .style(t.ghost(t.fg, false))
                    .on_press(msg);
                list = list.push(if r.folder {
                    b.into()
                } else {
                    menu_on(app.cwd.join(&r.path), b.into())
                });
            }
            return column![modes, field, list].spacing(10).into();
        }
        let found: Vec<&String> = app.found_files().collect();
        for f in found.iter().take(crate::files::LISTED) {
            list = list.push(menu_on(
                app.cwd.join(f.as_str()),
                button(
                    text(clip_start(f, cells(app, 40.0)))
                        .size(12)
                        .font(mono())
                        .color(t.fg)
                        .wrapping(text::Wrapping::None),
                )
                .width(Length::Fill)
                .padding([3, 8])
                .style(t.ghost(t.fg, false))
                .on_press(Msg::ViewFile(app.cwd.join(f.as_str()), None))
                .into(),
            ));
        }
        if found.len() > crate::files::LISTED {
            list = list.push(
                text(fill(w.files_more, "n", found.len() - crate::files::LISTED))
                    .size(12)
                    .color(t.fg_3),
            );
        }
        return column![modes, field, list].spacing(10).into();
    };
    let shown = short_path(&v.path.to_string_lossy(), &app.cwd);
    let small = |label: &'static str, msg: Msg| {
        button(text(label).size(11.5).color(t.fg_2).wrapping(text::Wrapping::None))
            .padding([3, 8])
            .style(t.ghost(t.fg_2, false))
            .on_press(msg)
    };
    let head = column![
        row![
            button(
                row![
                    icon(Icon::ChevronLeft, 11.0, t.fg_3),
                    text(w.files_back).size(12).color(t.fg_2)
                ]
                .spacing(4)
                .align_y(Alignment::Center)
            )
            .padding([3, 6])
            .style(t.ghost(t.fg_2, false))
            .on_press(Msg::CloseFile),
            Space::new().width(Length::Fill),
            small(
                s.open_editor,
                match v.line {
                    Some(line) => Msg::EditorAt(v.path.clone(), line),
                    None => Msg::Editor(v.path.clone()),
                }
            ),
            small(s.reveal, Msg::Reveal(v.path.clone())),
        ]
        .align_y(Alignment::Center),
        name(
            text(clip_start(&shown, cells(app, 30.0)))
                .size(12.5)
                .font(mono())
                .color(t.fg)
        ),
        // Under its name, what is done to the file itself: a row of its own, never pushed out.
        edit_buttons(app, v),
    ]
    .spacing(4);
    if let Some(why) = &v.error {
        return column![head, text(fill(w.files_unreadable, "why", why)).size(12.5).color(t.err)]
            .spacing(10)
            .into();
    }
    if v.image {
        return column![
            head,
            container(iced::widget::image(v.path.as_path()).content_fit(iced::ContentFit::ScaleDown))
                .width(Length::Fill)
                .padding(8)
                .style(t.boxed(t.raised, t.line, RADIUS)),
        ]
        .spacing(8)
        .into();
    }
    if let Some(edit) = app.file_edit.as_ref().filter(|e| e.path == v.path) {
        let token = v
            .path
            .extension()
            .or_else(|| v.path.file_name())
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let editor = iced::widget::text_editor(&edit.content)
            .on_action(Msg::FileEditAction)
            .font(mono())
            .size(12.5)
            .padding(8)
            .height(Length::Shrink)
            .highlight(
                &token,
                if t.dark {
                    iced::highlighter::Theme::Base16Ocean
                } else {
                    iced::highlighter::Theme::InspiredGitHub
                },
            )
            .key_binding(|press| {
                let save = matches!(&press.key, iced::keyboard::Key::Character(c) if c.as_str() == "s");
                if save && press.modifiers.command() {
                    Some(iced::widget::text_editor::Binding::Custom(Msg::FileSave))
                } else {
                    iced::widget::text_editor::Binding::from_key_press(press)
                }
            });
        return column![
            head,
            text(fill(w.edit_hint, "mod", super::MOD)).size(11.5).color(t.fg_3),
            editor
        ]
        .spacing(8)
        .into();
    }
    if let Some(md) = &app.file_preview {
        return column![
            head,
            container(super::timeline::reply(app, md, true))
                .width(Length::Fill)
                .padding(12)
                .style(t.boxed(t.raised, t.line, RADIUS)),
        ]
        .spacing(8)
        .into();
    }
    // The pointer over a line says it takes a comment; the box says what becomes of one.
    let mut notes = column![].spacing(2);
    if v.total > v.lines.len() {
        notes = notes.push(
            text(fill(&fill(w.files_cut, "shown", v.lines.len()), "n", v.total))
                .size(11.5)
                .color(t.warn),
        );
    }
    let spans = if t.dark { &v.dark } else { &v.light };
    // The review's findings on this file, looked up once.
    let found: Vec<(usize, &crate::review::Comment)> = app
        .chat
        .review
        .iter()
        .enumerate()
        .filter(|(_, c)| crate::review::same_file(&c.path, &v.path))
        .collect();
    let mut lines = column![];
    for (i, line) in v.lines.iter().enumerate() {
        let n = i + 1;
        let colored: Vec<iced::widget::text::Span<'_, (), iced::Font>> = spans
            .get(i)
            .into_iter()
            .flatten()
            .map(|(range, color)| {
                iced::widget::span(&line[range.clone()])
                    .color_maybe(color.or(Some(t.fg)))
                    .font(mono())
            })
            .collect();
        let marked = v.line == Some(n);
        let shown = container(
            row![
                text(n.to_string()).size(11).font(mono()).color(t.fg_3).width(40),
                iced::widget::rich_text(colored)
                    .size(12)
                    .font(mono())
                    .wrapping(text::Wrapping::WordOrGlyph),
            ]
            .spacing(4),
        )
        .width(Length::Fill)
        .padding([0, 6])
        .style(move |_| container::Style {
            background: marked.then(|| t.accent_wash.into()),
            ..container::Style::default()
        });
        lines = lines.push(
            iced::widget::mouse_area(shown)
                .interaction(iced::mouse::Interaction::Pointer)
                .on_press(Msg::CommentAt(v.path.clone(), n, false)),
        );
        for (i, c) in app.chat.comments.iter().enumerate() {
            if c.path == v.path && c.line == n {
                lines = lines.push(comment_note(app, i, c));
            }
        }
        for (i, c) in &found {
            if c.line == n {
                lines = lines.push(review_note(app, *i, c, false));
            }
        }
        if let Some(draft) = &app.commenting
            && draft.path == v.path
            && draft.line == n
        {
            lines = lines.push(draft_box(app, draft));
        }
    }
    column![
        head,
        notes,
        container(lines)
            .width(Length::Fill)
            .padding([4, 0])
            .clip(true)
            .style(t.boxed(t.raised, t.line, RADIUS)),
    ]
    .spacing(8)
    .into()
}

/// The buttons that edit the file read, or save it, and show a Markdown one as it reads.
fn edit_buttons<'a>(app: &'a App, v: &'a crate::files::Viewing) -> El<'a> {
    let (t, w) = (app.t, app.s.w());
    let small = |label: &'static str, msg: Option<Msg>, strong: bool| -> El<'a> {
        let b = button(text(label).size(11.5).wrapping(text::Wrapping::None))
            .padding([3, 8])
            .on_press_maybe(msg);
        if strong {
            b.style(t.primary()).into()
        } else {
            b.style(t.ghost(t.fg_2, false)).into()
        }
    };
    if v.error.is_some() || v.image {
        return Space::new().into();
    }
    if let Some(edit) = app.file_edit.as_ref().filter(|e| e.path == v.path) {
        // Saved by itself, it is closed, not left unsaved.
        if app.prefs.more.autosave {
            return row![
                text(w.edit_autosaving)
                    .size(11)
                    .color(t.fg_3)
                    .wrapping(text::Wrapping::None),
                small(w.edit_close, Some(Msg::FileEditCancel), false),
                small(w.edit_save, edit.changed().then_some(Msg::FileSave), true),
            ]
            .spacing(4)
            .align_y(Alignment::Center)
            .into();
        }
        return row![
            small(w.edit_cancel, Some(Msg::FileEditCancel), false),
            small(w.edit_save, edit.changed().then_some(Msg::FileSave), true),
        ]
        .spacing(4)
        .into();
    }
    let mut buttons = row![].spacing(4);
    if crate::edit::is_markdown(&v.path) {
        let previewing = app.file_preview.is_some();
        buttons = buttons.push(small(
            if previewing { w.preview_off } else { w.preview_on },
            Some(Msg::FilePreview(!previewing)),
            false,
        ));
    }
    buttons
        .push(small(w.edit_start, Some(Msg::FileEditStart), false))
        .into()
}

/// What a search of the files' contents found, each line to open at.
fn grep_list(app: &App) -> El<'_> {
    let (t, w) = (app.t, app.s.w());
    if app.grep_busy {
        return text(w.grep_busy).size(12.5).color(t.fg_3).into();
    }
    let Some(hits) = &app.grep_hits else {
        return Space::new().into();
    };
    if hits.is_empty() {
        return text(w.grep_none).size(12.5).color(t.fg_3).into();
    }
    let mut list = column![].spacing(1);
    for hit in hits {
        list = list.push(
            button(
                column![
                    text(format!("{}:{}", clip_start(&hit.path, cells(app, 40.0)), hit.line))
                        .size(11.5)
                        .font(mono())
                        .color(t.fg_3)
                        .wrapping(text::Wrapping::None),
                    text(hit.text.as_str())
                        .size(12)
                        .font(mono())
                        .color(t.fg)
                        .wrapping(text::Wrapping::None),
                ]
                .spacing(1),
            )
            .width(Length::Fill)
            .padding([3, 8])
            .style(t.ghost(t.fg, false))
            .on_press(Msg::ViewFile(app.cwd.join(&hit.path), Some(hit.line))),
        );
    }
    if hits.len() >= crate::edit::HITS {
        list = list.push(text(fill(w.grep_more, "n", crate::edit::HITS)).size(12).color(t.fg_3));
    }
    list.into()
}

/// A comment kept under the line it was made on, with the way to take it back.
fn comment_note<'a>(app: &'a App, i: usize, c: &'a crate::review::Comment) -> El<'a> {
    let t = app.t;
    container(
        row![
            icon(Icon::Pencil, 11.0, t.accent),
            text(c.text.as_str())
                .size(12)
                .color(t.fg)
                .width(Length::Fill)
                .wrapping(text::Wrapping::WordOrGlyph),
            button(icon(Icon::Close, 10.0, t.fg_3))
                .padding(2)
                .style(t.ghost(t.fg_3, false))
                .on_press(Msg::CommentRemove(i)),
        ]
        .spacing(6)
        .align_y(Alignment::Center),
    )
    .width(Length::Fill)
    .padding([4, 8])
    .style(t.fill(t.accent_wash))
    .into()
}

/// What the agent's review found: how many, the way to put them all away, and those on lines the
/// diff does not show, each with where it is.
fn review_found(app: &App) -> El<'_> {
    let t = app.t;
    let w = app.s.w();
    let g = &app.git;
    // Whether a line of a changed file is drawn in its diff, staged or not.
    let drawn = |c: &crate::review::Comment| {
        g.files
            .iter()
            .filter(|f| crate::review::same_file(&g.top.join(&f.path), &c.path))
            .any(|f| {
                [&f.staged_diff, &f.unstaged_diff]
                    .iter()
                    .any(|d| crate::git::numbers(d).into_iter().take(300).any(|n| n == Some(c.line)))
            })
    };
    let mut col = column![
        row![
            icon(Icon::Bot, 12.0, t.accent),
            text(fill(w.review_found, "n", app.chat.review.len()))
                .size(12)
                .color(t.fg_2)
                .width(Length::Fill),
            button(text(w.review_clear).size(11.5).color(t.fg_2))
                .padding([2, 8])
                .style(t.ghost(t.fg_2, false))
                .on_press(Msg::ReviewClear),
        ]
        .spacing(6)
        .align_y(Alignment::Center)
    ]
    .spacing(4);
    for (i, c) in app.chat.review.iter().enumerate() {
        if !drawn(c) {
            col = col.push(review_note(app, i, c, true));
        }
    }
    col.into()
}

/// A finding of the agent's review under its line: to go with the next message, or put away.
/// `placed` says where it is, for one shown away from its line.
fn review_note<'a>(app: &'a App, i: usize, c: &'a crate::review::Comment, placed: bool) -> El<'a> {
    let t = app.t;
    let w = app.s.w();
    let mut said = column![].spacing(2).width(Length::Fill);
    if placed {
        said = said.push(
            text(crate::review::place(&c.path, c.line, &app.cwd))
                .size(11)
                .font(mono())
                .color(t.fg_3),
        );
    }
    said = said.push(
        text(c.text.as_str())
            .size(12)
            .color(t.fg)
            .wrapping(text::Wrapping::WordOrGlyph),
    );
    container(
        row![
            icon(Icon::Bot, 11.0, t.warn),
            said,
            button(
                text(w.review_take)
                    .size(11)
                    .color(t.fg_2)
                    .wrapping(text::Wrapping::None)
            )
            .padding([1, 6])
            .style(t.ghost(t.fg_2, false))
            .on_press(Msg::ReviewTake(i)),
            button(icon(Icon::Close, 10.0, t.fg_3))
                .padding(2)
                .style(t.ghost(t.fg_3, false))
                .on_press(Msg::ReviewDrop(i)),
        ]
        .spacing(6)
        .align_y(Alignment::Center),
    )
    .width(Length::Fill)
    .padding([4, 8])
    .style(t.fill(t.warn_wash))
    .into()
}

fn plan_tab(app: &App) -> El<'_> {
    let (t, s) = (app.t, app.s);
    let Some(todos) = app.chat.transcript.plan().filter(|p| !p.is_empty()) else {
        return empty(app, Icon::List, s.pl_empty, s.pl_empty_p);
    };
    let done = todos.iter().filter(|todo| todo.status == TodoStatus::Completed).count();
    // How far along, as a bar: the done part and the rest share the width by their counts.
    let mut bar = row![].height(4);
    if done > 0 {
        bar = bar.push(
            container(Space::new())
                .width(Length::FillPortion(done as u16))
                .height(4)
                .style(t.tag(t.accent, t.accent)),
        );
    }
    if done < todos.len() {
        bar = bar.push(
            container(Space::new())
                .width(Length::FillPortion((todos.len() - done) as u16))
                .height(4)
                .style(t.tag(t.line, t.line)),
        );
    }
    column![
        text(fill(&fill(s.pl_done, "a", done), "b", todos.len()))
            .size(12.5)
            .color(t.fg_2),
        bar.spacing(2),
        container(todo_list(t, todos)).padding([6, 2]),
    ]
    .spacing(8)
    .into()
}

fn commands_tab(app: &App) -> El<'_> {
    let (t, s) = (app.t, app.s);
    // How much of a command its card's head has room for, next to how it ended and how long it took.
    let head_cells = cells(app, 200.0);
    let mut col = column![].spacing(10);
    if !app.script_runs.is_empty() {
        col = col.push(script_runs(app));
    }
    if app.chat.transcript.commands().next().is_none() {
        return col.push(empty(app, Icon::Terminal, s.cm_empty, s.cm_empty_p)).into();
    }
    for tool in app.chat.transcript.commands() {
        let command = tool.arg("command").unwrap_or(&tool.summary);
        let state = match (tool.state, tool.killed.as_deref(), tool.exit_code) {
            (ToolState::Running, _, _) => tag(t, s.running, t.accent_wash, t.accent),
            (_, Some("timeout"), _) => tag(t, s.killed_timeout, t.err_wash, t.err),
            (_, Some(_), _) => tag(t, s.killed_abort, t.inset, t.fg_2),
            (_, None, Some(0)) => tag(t, fill(s.exit, "n", 0), t.inset, t.fg_2),
            (_, None, Some(code)) => tag(t, fill(s.exit, "n", code), t.err_wash, t.err),
            (ToolState::Failed, None, None) => tag(t, s.tag_failed, t.err_wash, t.err),
            _ => Space::new().into(),
        };
        let mut head = row![
            text("$").size(12.5).font(mono()).color(t.fg_3),
            name(
                text(clip(first_line(command), head_cells))
                    .size(12.5)
                    .font(mono())
                    .color(t.fg)
            ),
            state,
        ]
        .spacing(8)
        .align_y(Alignment::Center);
        if let Some(d) = tool.elapsed.filter(|d| d.as_millis() >= 1000) {
            head = head.push(
                text(s.duration(d))
                    .size(11.5)
                    .color(t.fg_3)
                    .wrapping(text::Wrapping::None),
            );
        }
        let mut body: Vec<El<'_>> = Vec::new();
        // The whole command when the head had to cut it.
        if command.lines().count() > 1 || clip(command, head_cells) != command {
            body.push(
                container(
                    text(command)
                        .size(12)
                        .font(mono())
                        .color(t.fg)
                        .wrapping(text::Wrapping::WordOrGlyph),
                )
                .width(Length::Fill)
                .padding([6, 10])
                .style(t.boxed(t.raised, t.line, RADIUS))
                .into(),
            );
        }
        // Nothing written yet is not "no output" while the command still runs.
        if tool.state != ToolState::Running || !tool.shown_output().is_empty() {
            body.push(output_block(t, s, tool.shown_output(), 60));
        }
        if tool.arg("command").is_some() {
            body.push(command_copies(app, tool));
        }
        let body = (!body.is_empty()).then(|| Column::with_children(body).spacing(6).into());
        col = col.push(card(app, &tool.id, head.into(), body));
    }
    col.into()
}

fn usage_tab(app: &App) -> El<'_> {
    let (t, s) = (app.t, app.s);
    let u = &app.chat.transcript.usage;
    let item = |label: &'static str, value: String| {
        row![
            text(label).size(13).color(t.fg_2),
            Space::new().width(Length::Fill),
            text(value).size(13).font(mono()).color(t.fg),
        ]
        .padding([5, 2])
        .align_y(Alignment::Center)
    };
    let tokens = |n: u64| {
        if n < 1000 {
            n.to_string()
        } else {
            format!("{} ({n})", compact(n))
        }
    };
    column![
        text(s.us_head).size(12).color(t.fg_3),
        container(
            column![
                item(s.us_in, tokens(u.input)),
                hline(t),
                item(s.us_out, tokens(u.output)),
                hline(t),
                item(s.us_cr, tokens(u.cache_read)),
                hline(t),
                item(s.us_cw, tokens(u.cache_write)),
                hline(t),
                item(s.us_req, app.chat.transcript.requests.to_string()),
                hline(t),
                item(s.us_last, tokens(app.chat.transcript.last_input)),
                hline(t),
                item(s.us_cost, format!("${:.4}", u.cost)),
            ]
            .padding([4, 10])
        )
        .style(t.boxed(t.raised, t.line, RADIUS_L)),
        text(s.us_note).size(12).color(t.fg_3),
        // Every conversation's, by day and by model: the screen `/usage` opens.
        button(
            row![
                icon(Icon::Gauge, 13.0, t.fg_2),
                text(s.w().usage_open).size(12.5).color(t.fg),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        )
        .width(Length::Fill)
        .padding([6, 10])
        .style(t.outline(t.fg))
        .on_press(Msg::UsagePart(crate::usage::Part::All)),
    ]
    .spacing(10)
    .into()
}
