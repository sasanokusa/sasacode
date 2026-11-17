# TUI: what pi-tui does for sasacode, and how to port it to ratatui + crossterm

Researched 2026-11-18 for `crates/sasacode-tui` (ADR-010 picks ratatui + crossterm; the workspace already pins
`ratatui 0.30` with `crossterm_0_29`, `crossterm 0.29` with `bracketed-paste`, `unicode-width 0.2`).

Sources and short names used below:
- `app.ts:N`, `views.ts:N`, … = `packages/tui/src/*.ts`; tests in `packages/tui/test/*.test.ts`.
- `P/x.js:N` = `@earendil-works/pi-tui` **0.87.1** `dist/`, *patched* (`patches/@earendil-works%2Fpi-tui@0.87.1.patch`),
  in the main checkout at `node_modules/.bun/@earendil-works+pi-tui@0.87.1/node_modules/@earendil-works/pi-tui/dist`
  (the worktree has no `node_modules`). Line numbers are of the patched dist.
- Rust sources read from `~/.cargo/registry/src/index.crates.io-*/`: `ratatui-core-0.1.2` (what `ratatui 0.30.2`
  re-exports), `ratatui-crossterm-0.1.2`, `crossterm-0.29.0`, `unicode-width-0.2.3`, `unicode-segmentation-1.13.3`.
  Newest on crates.io (`cargo search`): ratatui 0.30.2, crossterm 0.29.0, unicode-width 0.2.3,
  unicode-segmentation 1.13.3, pulldown-cmark 0.13.6, vt100 0.18.1, insta 1.49.0, icu_segmenter 2.4.0.
  ratatui-core and ratatui-widgets depend on `unicode-width >=0.2.0`, so the workspace's unicode-width version
  decides ratatui's widths too.
- **Versions to use**: `ratatui = "0.30.2"` (features `crossterm_0_29`, as pinned), `crossterm = "0.29.0"`
  (`bracketed-paste`, `event-stream`), `unicode-width = "=0.2.3"`, `unicode-segmentation = "1.13"`,
  `pulldown-cmark = "0.13"`; dev: `vt100 = "0.18"`, `insta = "1.49"`. Optional: `icu_segmenter = "2.4"` (§2).

## 0. Design in one paragraph

Keep pi-tui's model, not ratatui's widget model: every component renders `Vec<StyledLine>` at a given width
(pi-tui: `render(width): string[]`), the app composes those lines (transcript in a scroll view, then status,
input and footer), and only the final screen is copied into a ratatui `Buffer`. Use ratatui as the cell buffer,
the diff, the crossterm backend and `TestBackend`; do **not** use `Paragraph` wrapping, `Line::width`,
`Buffer::set_string*` or tui-textarea for sasacode text. Reasons, all measured (§3): pi-tui's width rules differ
from unicode-width's (tab 3 vs 1, controls 0 vs 1, a lone regional indicator 2 vs 1, ambiguous letters under
`width_cjk`), its wrapping breaks CJK per character and carries styles across lines, and caches keyed by width
(`LineCache`, `Padded`) are what keep a 50,000-line transcript fast (`views.ts:33-49`). This also lets
`views.test.ts` port 1:1 (render at a width, compare plain lines). Write cells yourself with the width you
computed and mark them with `CellDiffOption::ForcedWidth` (ratatui-core `buffer/cell.rs:12-32`, used by the diff
in `buffer/diff.rs` and by `Cell::cell_width`, `cell.rs:312-317`) so ratatui's diff and the crossterm backend's
`MoveTo` logic (`ratatui-crossterm/src/lib.rs:241-247`) follow the same widths as the layout. Run (scratch program,
ratatui 0.30.2, `CrosstermBackend` into a byte sink): `①` with `ForcedWidth(2)` at x=0 and `a` at x=2 is written as
`ESC[1;1H① ESC[1;3Ha` (an explicit move); the same cells without it (what `set_string` does, `①` at 0, `a` at 1)
are written as `ESC[1;1H①a`, so on an ambiguous-wide terminal `a` lands one column late. Replacing the forced `①`
by `ab` next frame writes `ESC[1;1Hab`; a cell holding `ESC]8;;URL ESC\ab ESC]8;; ESC\` with `ForcedWidth(2)`
followed by `c` gets `ESC[1;3Hc`, so OSC 8 links work the same way. (The diff re-emits the trailing column of a
shrunk wide cell only when it had a background or reverse/underline/blink, `buffer/diff.rs:165-182`; otherwise it
relies on the terminal erasing a wide glyph that is overwritten, as for any CJK text in ratatui.)

## 1. pi-tui components sasacode uses, and the rendering model

| pi-tui | Used at | What sasacode relies on |
| --- | --- | --- |
| `TuiAltScreen` | `app.ts:144-151` | Full screen (default, `config.tui.altScreen !== false`), mouse on, wheel 3 lines, drag-select copies via `copyToClipboard`, scroll-to-end label `" ↓ 最新へ (ctrl+end) "` inverse. `showHardwareCursor=false` (2nd ctor arg). |
| `TuiMainScreen` | `app.ts:152` | `altScreen:false`: inline rendering into scrollback (also what most app tests run, `app.test.ts:66,78`). |
| `ProcessTerminal` / `Terminal` | `app.ts:101` | Raw mode, bracketed paste, kitty/modifyOtherKeys negotiation, resize; tests substitute a fake. |
| `VStack`, `ScrollView` | `app.ts:172-184` | Layout root: `ScrollView(transcript,{follow:"end",primary:true,overscroll:"chain",scrollbar:"auto"})` grows; dock = status (min 0), input (min 3), footer (min 0). |
| `Container`, `Spacer` | `app.ts:108-111,440` | Child lists; `Spacer(1)` before each message/notice. |
| `Text` | `views.ts:22,233`, `activity.ts:85` | Wrap with `wrapTextWithAnsi`, tabs→3 spaces, pad each line to width, `paddingX`, empty/whitespace text renders **zero lines** (`P/components/text.js:37-88`). |
| `Markdown` | `views.ts:97`, `text-viewer.ts:24` | marked-based renderer with `transform` hook (`P/components/markdown.js:186`), cached by text+width. §4. |
| `Editor` | `app.ts:156-158` | Multi-line input, `paddingX:1`, history, paste markers, autocomplete. §2. |
| `CombinedAutocompleteProvider` | `app.ts:234-254` | Slash commands + their argument completions + Tab path completion, cwd-based. |
| `SelectList` | `dialogs.ts:86,167` | `"→ "` selected prefix (Picker finds the row by it, `dialogs.ts:118`), wraps around on ↑/↓, primary column 24–64 cols, `"  (n/m)"` scroll info (`P/components/select-list.js:58`). |
| `Input` | `dialogs.ts:179,248`, `text-viewer.ts:43` | One-line field with the editor's keys, cursor marker, placeholder. |
| `matchesKey`, `getKeybindings` | `app.ts:154,302-342` | Key ids like `"ctrl+shift+f"`; user bindings `tui.altScreen.top/bottom = ctrl+home/ctrl+end` (home/end stay in the input). |
| `truncateToWidth`, `visibleWidth`, `wrapTextWithAnsi`, `stripTerminalSequences` | everywhere | §3. |
| `setAmbiguousWidth`, `setAltScreenSearchText` | `ambiguous.ts:11-13`, `app.ts:155,729` | Added by the patch (§6). |

Not used: `Loader`/`CancellableLoader` (Activity is a `Text` re-rendered by its own 80 ms timer, `activity.ts:6-7,115`),
`Box`, `HStack`, `SettingsList`, `Image`, overlays other than the search box, OSC 11 / colour-scheme queries.

**Full-screen frame pipeline** (`P/tui-alt-screen.js:1431-1509`): lay out the root at terminal width×height
(`renderLayoutFrame`), strip OSC 133 prefixes, paint search highlights, the scroll-to-end label, overlays,
keep the last `height` rows, paint the mouse selection and "Copied!" flashes, extract the cursor marker, append
`SEGMENT_RESET` (`\x1b[0m\x1b]8;;\x07`, `P/tui.js:131`) to every line, hard-slice any line wider than the screen
(`sliceByColumn`), then write **one** `\x1b[?2026h … \x1b[?2026l` block: full redraw (`\x1b[2J`) on the first
frame or a size change, otherwise only rows whose string changed, each as `\x1b[row;1H\x1b[2K<line>`. Finally the
cursor is moved to the marker and hidden (`\x1b[?25l`) because `showHardwareCursor` is false (`:1496-1502`).
Scheduling: `requestRender()` coalesces to at most one frame per 16 ms (`P/tui.js:169,601-650`); a key handled by
the focused component renders immediately (`P/tui.js:724-727`); `requestRender(true)` forces a full redraw
(used after ctrl+o and session resets, `app.ts:335,1054`).

**Layout and margins** (`app.ts:160-186`): one `Padded` (`views.ts:253-284`) gives 2 columns left/right to the
transcript, status, input and footer. The "1 line top" is a blank line *inside the scroll content* above the
header (it scrolls away); the "1 line bottom" is a fixed blank line under the footer. `Padded` keeps an inner
width of at least 10 (`views.ts:266`), strips cursor-moving controls from every child line (`safeLine`,
`views.ts:244-248`) and keeps an OSC 133 prompt mark at column 0 (`views.ts:250,274-275`). Input dialogs get
`rows - status - footer - 3` rows (`app.ts:959-962`).

**Start/stop order** (`P/tui.js:537-548,581-591`, `P/tui-alt-screen.js:145-219`, `P/terminal.js:107-139,331-370`):
start = `beforeTerminalStart` writes `\x1b[?1049h\x1b[?7l` + mouse modes + `\x1b[2J\x1b[H\x1b[?25l`, *then*
`terminal.start()` enables raw mode, `\x1b[?2004h` and pushes the kitty flags (`\x1b[>7u\x1b[?u\x1b[c`), so the
keyboard mode is set **inside** the alternate screen. Stop = `beforeTerminalStop` (mouse off, `\x1b[?7h`), show
cursor, `terminal.stop()` (`\x1b[?2004l`, kitty pop `\x1b[<u`, `\x1b[>4;0m`, raw mode restored), *then*
`afterTerminalStop` leaves with `\x1b[?1049l\x1b[?25h` (sasacode passes `preserveScreen:true` in full screen so
the transcript is *not* replayed onto the main screen, `app.ts:224`). The kitty keyboard protocol keeps separate
flag stacks for the main and alternate screens, so a push made before `1049h` would not apply inside the TUI
(and a pop after `1049l` would pop the wrong stack). Then sasacode writes `\r\x1b[2K` + the usage summary and
`sasacode -r <id>` (`app.ts:227`, `summary.ts:67-72`). No signal or panic handler restores the terminal in TS.

**Inline mode** (`P/tui-main-screen.js:212-593`): renders the whole document every frame and diffs it against
the previous lines; a change above the visible viewport, a width change or a height change triggers a full
redraw that clears the scrollback (`\x1b[2J\x1b[H\x1b[3J`, `:244,295-311,408`). Because ctrl+o and tool status
rewrite old entries, this mode routinely rewrites history. In Rust, implement it with the same line model and
our own row diff (ratatui's `Viewport::Inline` + `insert_before` cannot rewrite lines already pushed into
scrollback). Keep it, since the app tests use it and the README documents `"tui": { "altScreen": false }`
(README.md:109).

## 2. The editor

**Keys** (defaults in `P/keybindings.js:2-140`; dispatch `P/components/editor.js:556-821`; sasacode's own keys in
`app.ts:275-344`, which run *after* the full-screen viewport listener and *before* the focused component):

| Key | Effect |
| --- | --- |
| enter | submit (`editor.js:738-760`); with a `\` before the cursor: delete it, newline instead (terminals without shift+enter) |
| shift+enter, ctrl+j, alt+enter (`\x1b\r`), `\x1b[13;2~`, any multi-byte `\x1b…\r` | newline (`editor.js:723-737`) |
| ↑ / ↓ | move by *visual* line; on the first visual line ↑ browses history when the input is empty, already browsing, or the cursor is at column 0, else jumps to line start; ↓ on the last visual line goes forward in history while browsing, else to line end (`editor.js:754-786`) |
| ← → / ctrl+b ctrl+f | char; alt+←→, ctrl+←→, alt+b alt+f: word |
| home end ctrl+a ctrl+e | line start/end (ctrl+home/end are taken by the transcript in full screen, `app.ts:154`) |
| ctrl+k / ctrl+u | delete to line end / start (kill ring) |
| ctrl+w, alt+backspace / alt+d, alt+delete | delete word back / forward |
| ctrl+y / alt+y | yank / yank-pop; ctrl+- undo (fish-style coalescing, `editor.js:1012-1024`) |
| ctrl+] / ctrl+alt+] | jump to next typed character forward / backward |
| pageup/pagedown | editor page scroll, but in full screen the viewport listener takes them first (`P/tui-alt-screen.js:539-549`) |
| delete, ctrl+d | delete forward; ctrl+d on an empty input while idle exits (`app.ts:328-331`) |
| tab | complete (§ below); shift+tab cycles the permission mode (`app.ts:338-342`) |
| esc | close autocomplete; otherwise, while running, interrupt and give queued messages back after the draft (`app.ts:309-317,989-999`, uses patched `moveToEnd`) |
| ctrl+c | running: interrupt; text: clear; else twice within 1.5 s exits (`app.ts:318-327`). The editor ignores it (`editor.js:601`). |
| ctrl+o | toggle `display.expanded` (tool output, thinking, diffs), invalidate, full redraw (`app.ts:332-337`) |
| ctrl+v | if a paste handler (image plugin) exists, ask it with `""` (`app.ts:302-305`) |
| backspace/delete at `[Image #n]` | deletes the whole marker by feeding the editor synthetic keys (`app.ts:350-364`); paste markers are atomic in the editor itself |

Release events are dropped before components see them (`P/tui.js:720-722`, kitty flag 2 reports them).

**History**: the editor keeps at most 100 entries, newest first, no consecutive duplicates, trimmed
(`editor.js:316-328`). sasacode loads the last 200 lines of a JSONL file (one JSON string per line) and appends each
submit with mode 0600 (`app.ts:1107-1125`), so only the newest 100 are reachable. Browsing saves the draft and
restores it when you come back past the newest entry (`editor.js:342-373`).

**Paste pipeline** (two layers):
1. `ProcessTerminal`'s stdin buffer reassembles a whole bracketed paste and re-wraps it as one
   `\x1b[200~…\x1b[201~` string (`P/terminal.js:156-160`, `P/stdin-buffer.js:267-305`). sasacode's `onKey` still
   handles a paste split over several chunks (tests send it split, `app.test.ts:375-376`) and text before/after it.
2. sasacode offers the text to plugin paste handlers first (image paths become `[Image #n] `), then: ≤40 lines and
   ≤4,000 chars → inserted as typed text after `\r\n?→\n`, tab→4 spaces, controls dropped; longer → handed to
   the editor as a bracketed paste, shown once with a notice (`app.ts:69-71,367-392`). The editor folds a paste over
   10 lines or 1,000 chars into `[paste #N +L lines]` (more than 10 lines) or `[paste #N C chars]`, decodes
   tmux's CSI-u ctrl letters inside pastes, drops controls, prefixes a space before a path pasted right after a
   word character (`editor.js:1065-1120`). Markers are atomic for cursor and delete (`segmentWithMarkers`,
   `editor.js:31-86`). On submit they are expanded **only if exactly as inserted** (patch, `editor.js:908-917`)
   and the whole text is trimmed (`editor.js:1151-1165`). `setText` forgets folded pastes, so the interrupt path
   appends with `moveToEnd` + `insertTextAtCursor` instead (`app.ts:993-997`).

**Autocomplete** (`P/autocomplete.js:187-345`, editor glue `editor.js:1012-1063,1888-1980`): typing `/` at the
start of the message opens the command list (fuzzy, `P/fuzzy.js:78`); letters keep it updated; after
`/cmd ` the command's `getArgumentCompletions` runs (sasacode returns `[]` once the argument is typed out exactly,
so enter runs instead of completing, `app.ts:243-248`, issue #114). Enter on a slash-command item completes *and
submits*; on an argument item it only completes (`editor.js:631-648`). Tab with no list forces path completion
from `readdirSync(cwd)`. `@` and `#` are trigger characters, but the provider is built without an `fd` path
(`app.ts:236-252`, default `fdPath=null` at `autocomplete.js:191`) and `@` fuzzy search returns `[]` without it
(`autocomplete.js:592-594`): **@-mentions are effectively off**; port them as off. List layout: slash commands
12–32 columns, arguments 12–64 (patch); at most 5 rows (`editor.js:236`).

**IME and the cursor**: the focused editor/Input inserts `CURSOR_MARKER` (`\x1b_pi:c\x07`, an APC, zero width,
`P/tui.js:54`) before the fake inverse-video cursor cell (`editor.js:438-461`, `P/components/input.js:354,406`).
The renderer finds it in the visible rows, computes the column with `visibleWidth` of the text before it
(`P/tui.js:979-1000`) and moves the real cursor there every frame, hidden. Terminals put IME preedit/candidates at
the real cursor, so Japanese input appears in the input box. `docs/dependencies.md:35` notes the candidate window
position was never verified automatically. In Rust: compute the cursor cell from the same width function; draw
the frame *without* `Frame::set_cursor_position` (ratatui then hides the cursor) and afterwards call
`Terminal::set_cursor_position(pos)`, which only moves it (`ratatui-core/src/terminal/cursor.rs:80-85`), both inside
the synchronized block. Setting the frame cursor would *show* it (`terminal/render.rs:288-303`). This way
`TestBackend::cursor_position()` reports the IME position and `cursor_visible()` stays false.

**Wide characters in the editor**: cursor columns are JS string indices (UTF-16) moved by grapheme
(`Intl.Segmenter`); the editor shows at most `max(5, 30% of rows)` lines and scrolls with `↑ n more` borders
(`editor.js:414`), and the cursor may overflow into the 1-column padding (`editor.js:401-494`). Visual lines come
from the editor's own `wordWrapLine` (`editor.js:87-169`), not `wrapTextWithAnsi`: plain text only, chunks keep
their start/end index into the logical line (↑/↓, the sticky column and the IME column depend on this), trailing
spaces are *kept* (no `trimEnd`), it breaks after the last space or on either side of a CJK grapheme, and a paste
marker is one atomic grapheme (split only visually when wider than the line). In Rust use byte offsets on grapheme
boundaries (unicode-segmentation). Word movement uses ICU word segmentation, which is *dictionary based* for
Japanese: `日本語|の|テキスト|を|編集|する`, while unicode-segmentation's UAX #29 gives
`日|本|語|の|テキスト|を|編|集|す|る` (both run). For parity of ctrl+w / alt+←→ on Japanese use `icu_segmenter` 2.x
(`WordSegmenter` with its CJ dictionary data; binary size cost not measured) or accept the difference and record
it in `rust/MAP.md`.

## 3. Width, wrapping, truncation

**pi-tui's width** (`P/utils.js:154-263`): strip CSI/OSC/APC; split into graphemes (`Intl.Segmenter`); per
grapheme: tab = 3; spacing marks count; zero-width clusters (controls, marks, default-ignorables) = 0; an
RGI emoji (incl. ZWJ, modifier, VS16 sequences) = 2; a regional indicator = 2 even alone; else
`get-east-asian-width` 1.6.0 of the first code point (W/F = 2, A = 1 or 2 per `setAmbiguousWidth`), plus extra
for trailing halfwidth/fullwidth forms and Thai/Lao AM. Pure printable ASCII short-circuits; results are cached
(512 entries, cleared when the ambiguous width changes).

**Measured differences** (every row run: the patched pi-tui `visibleWidth` under Bun with `setAmbiguousWidth(1)`
/ `(2)`, and a Rust program on unicode-width 0.2.3 `str::width` / `width_cjk`):

| Text | pi-tui (amb 1 / 2; one value = both) | `width()` | `width_cjk()` | Note |
| --- | --- | --- | --- | --- |
| `\t` | 3 | 1 | 1 | char width is `None`; str counts every control as 1 |
| `\x1b`, `\x01`, `\n` | 0 | 1 | 1 | strip escapes and controls before measuring |
| lone `🇯` | 2 | 1 | 1 | pi-tui is conservative for streaming flags |
| `🇯🇵` `👍🏽` `👨‍👩‍👧` `❤️` `⚠️` | 2 | 2 | 2 | same |
| `❤` `☺` `⚠` (no VS16) | 1 | 1 | 1 | same |
| `①` `○` `●` `→` `…` `·` `─` `∴` `※` `↓` | 1 / 2 | 1 | 2 | ambiguous: same if mode follows the probe |
| `α` `Д` `é` (ambiguous *letters*) | 1 / **2** | 1 | **1** | `width_cjk` keeps letters narrow (unicode-width rule 6, `lib.rs:116-123`) |
| `⎿` `⠋` `␛` `↵` `↻` `✓` | 1 | 1 | 1 | glyphs sasacode draws |

So: ambiguous=1 matches `width()` once controls, tabs and lone regional indicators are special-cased; ambiguous=2
does **not** match `width_cjk()` for Greek, Cyrillic and Latin-1 letters. What an ambiguous-wide terminal really
draws for those is terminal-specific and the probe only tests `①`; for parity generate an East_Asian_Width=A
table (from `EastAsianWidth.txt` at build time) and use it when the probe says 2, or accept `width_cjk` and record
the deviation. Note ratatui's own `cell_width` debug-asserts on a single ASCII control symbol
(`ratatui-core/src/buffer/cell_width.rs`), another reason to sanitize first.

**The ambiguous probe** (`ambiguous.ts:9-45`, before `tui.start()`, `app.ts:206`): unless
`SASACODE_AMBIGUOUS_WIDTH=1|2`, print `\r①\x1b[6n` in raw mode and read `\x1b[r;cR`; column 3 → 2 cells; no
answer in 400 ms → keep 1; then `\r\x1b[2K`. crossterm's `cursor::position()` waits up to 2,000 ms
(`crossterm-0.29.0/src/cursor/sys/unix.rs:39`) and its DSR reply is an internal event, so do the probe with
our own read of the tty (poll with a 400 ms timeout) *before* crossterm's event reader starts.

**Wrapping** (`wrapTextWithAnsi`, `P/utils.js:699-866`): split each input line into tokens: runs of spaces, runs
of non-space, and **every Han/Hiragana/Katakana/Hangul/Bopomofo grapheme as its own token** (`cjkBreakRegex`,
`utils.js:51`), so CJK breaks between any two characters, with no kinsoku (a `。` can start a line). A token wider
than the line is broken by grapheme. Wrapped lines are `trimEnd`ed and never start with the space that caused the
break. Active SGR state is re-emitted at the start of each continuation line and across `\n`; only underline is
reset at a line end (to keep backgrounds). Measured: `wrap("hello 日本語のテキストです。", 8)` →
`["hello 日","本語のテ","キストで","す。"]`. Use this, not `Paragraph::wrap` or unicode-linebreak, or snapshots
diverge.

**Truncation** (`truncateToWidth`, `P/utils.js:971+`): default ellipsis is ASCII `"..."` (sasacode never passes
another), styles are closed with `\x1b[0m` around the ellipsis, optional padding. Measured:
`truncate("日本語のテキスト", 7)` = `日本\x1b[0m...\x1b[0m`. sasacode's own `clip()` cuts by grapheme count and adds
`…` (`app.ts:60-63`).

**Making text safe** (keep all three): `plain()` turns ESC into `␛` and drops C0 (except `\n`,`\t`), DEL, C1 for
anything from the model, tools, MCP or files (`views.ts:18-20`); `oneLine()` for tool headers (`views.ts:202-204`);
`safeLine()` in `Padded` drops cursor-moving controls from every rendered line and turns tabs into 3 spaces
(`views.ts:244-248`). The approval dialog warns when the command contains controls (`dialogs.ts:162-164`).

**Crates**: `unicode-width = "0.2.3"` (default `cjk` feature) as the base table; `unicode-segmentation = "1.13.3"`
for graphemes; our own `text::width` on top (rules above, emoji via unicode-width's sequence handling, which
matched pi-tui for every sequence tested). Since ratatui depends on `unicode-width >=0.2.0`, upgrading it moves
both our widths and ratatui's; pin it exactly (`=0.2.3`) and bump on purpose.

## 4. Markdown

pi-tui parses with `marked` (GFM) plus two extensions: strict `~~strike~~` (`P/components/markdown.js:5-20`) and
LaTeX (`$…$`, `$$…$$`, `\(…\)`, `\[…\]`, registered unconditionally at `:149`, rendered unless
`renderLatex:false`, `:369-381,492-498`; `P/latex.js` is 1,377 lines of TeX→Unicode). sasacode passes no options
but `transform`, so **LaTeX rendering is on**. Decide port (pulldown-cmark has `ENABLE_MATH` for the delimiters,
the renderer must still be ported) or drop (record in `rust/MAP.md`).

Output shape (`markdown.js:330-470,589-615,663-806`): headings: h1 bold+underline+cyan, h2 bold cyan, h3+ keep
the `### ` prefix; a blank line after headings, paragraphs (not before a list), code blocks, tables. Code blocks:
a gray line `` ```lang ``, the code indented 2 with no highlighting (`highlightCode` unset), a gray closing
`` ``` ``. Lists: `- ` / `N. ` (respecting `start`), task items `[x] `, nested lists indented. Tables: box drawing
`┌─┬─┐ │ ├─┼─┤ └─┴─┘` with width-aware column sizing and cell wrapping (long words up to 30 columns kept whole);
too narrow → the raw markdown wrapped. Blockquote: gray `│ ` + italic. Rule: gray `─` × min(width, 80). Inline:
bold, italic, `code` yellow, strike, links. Every line is then wrapped with `wrapTextWithAnsi`; results cached by
text+width (`markdown.js:179-186`). Colours come from `theme.ts:6-54` (basic 16 colours and 256-colour
backgrounds `236` user, `22` add, `52` delete).

**Links**: `link` text in blue+underline. If the terminal is known to support OSC 8 it is emitted as
`\x1b]8;;URL\x1b\\text\x1b]8;;\x1b\\` and the URL is not shown; otherwise `text (url)` in gray unless text equals
the URL (`markdown.js:529-551`). Detection is environment-only (`P/terminal-image.js:36-92`): kitty, Ghostty,
WezTerm, Warp, iTerm2, Windows Terminal (`WT_SESSION`), Alacritty, VS Code, Zed → yes; tmux only if
`tmux display -p '#{client_termfeatures}'` lists `hyperlinks`; screen, JetBrains, Apple Terminal, unknown → no;
`PI_HYPERLINKS=1|0` overrides. sasacode rewrites link targets *before* parsing (`links.ts:32-63`): outside fenced
code and code spans, `[t](path)` with a relative, `~/`, absolute or `file:line[:col]` target becomes a
`file://` URL against the cwd; a `#fragment` and a `:line` suffix are dropped (a file URL has no line); the
fragment is cut **before** percent-decoding and decoding uses `decodeURIComponent`, so `foo%23bar.md` is the
file `foo#bar.md`, whose URL `pathToFileURL` writes as `file:///…/foo%23bar.md` (issues #115, #138,
`links.test.ts:54-68`); `tel:`/`sms:`/`fax:`/`callto:` with digits stay URLs; replacement is literal (`$&`, `$$`
in paths). `url::Url::from_file_path` + `percent-encoding` should give the same URLs (not verified: port
`links.test.ts` first and compare, including Windows paths). Clicking a link inside sasacode does nothing
(`openUrl` is not passed, `P/tui-alt-screen.js:100,1099-1110`); with mouse capture on, the terminal opens it only
with its own modifier-click. The pager strips OSC sequences and keeps only SGR (`text-viewer.ts:67-69`).

In Rust: `pulldown-cmark 0.13` with `ENABLE_TABLES | ENABLE_STRIKETHROUGH | ENABLE_TASKLISTS` (+`ENABLE_MATH` if
LaTeX stays). Keep the hyperlink as a span attribute; when writing cells, wrap each linked cell's symbol in the
OSC 8 open/close and give it `ForcedWidth` of its grapheme width (the escape bytes would otherwise count).

## 5. Terminal features

| Feature | TS behaviour | Rust (crossterm 0.29) |
| --- | --- | --- |
| Alternate screen | `\x1b[?1049h`, autowrap off `\x1b[?7l` (`P/tui-alt-screen.js:11-14,181`) | `EnterAlternateScreen`, `DisableLineWrap` (`crossterm/src/terminal.rs:159`). |
| Synchronized output | every frame in `\x1b[?2026h/l`, also stop (`:20-21,1474,1504`) | `BeginSynchronizedUpdate`/`End…` (`terminal.rs:433`) around `terminal.draw` + cursor move. |
| Mouse | `1000+1002+1004+1006`, plus `1003` (all motion) unless `TMUX`/`ZELLIJ`/`STY`/`TERM` tmux*/screen* (`:15-17,170-181`) | `EnableMouseCapture` always sends 1003 and 1015 (`crossterm/src/event.rs:325-333`); write the raw modes ourselves to keep the tmux rule. |
| Focus | 1004 on; focus-out cancels a drag (`:462-490`) | `Event::FocusLost/Gained`. |
| Wheel | 3 lines, ×5 with alt (`app.ts:147`, `:784-787`); routed to the scroll view under the pointer, chaining to the transcript (`:788-802`) | `MouseEventKind::ScrollUp/Down` + modifiers. |
| Selection | left drag selects within the scroll content (not just the screen), auto-scrolls past the edges, double-click word (`/` and `-` join), triple-click line; release copies via `copySelection` then flashes English `"Copied!"` (`:953-1232,1415-1430`). Native selection needs option/alt (`app.ts:707`). | Own implementation over the transcript's line model (plain text + cell→byte map). |
| Scrollbar | `"auto"`: shown on scroll activity, hidden 1 s later, draggable, hover (`P/components/scroll-view.js:35,47-93`, `:839-912`) | Paint in the rightmost column; ratatui `Scrollbar` widget is fine for drawing only. |
| Viewport keys | pageup/pagedown = viewport−4; ctrl(+shift)+↑/↓ = previous/next user prompt (OSC 133;A rows, `views.ts:29`, `:289-303`); ctrl+home/end top/bottom (`app.ts:154`); ctrl+shift+f search (`:462-592`) | Same keymap before the focused component; store prompt rows as metadata, never emit OSC 133 in full screen (stripped at `:1443`). |
| Search | overlay top-right, 40% width, min 32, margin 1; enter / ctrl+g next, shift+enter / ctrl+shift+g previous, esc close; matches underlined, current bold+inverse; texts from `setAltScreenSearchText` (`:304-326,1233-1317`) | Overlay component; search over plain transcript lines. ctrl+shift+f is only distinct from ctrl+f with kitty keys or modifyOtherKeys. |
| Bracketed paste | `\x1b[?2004h` (`P/terminal.js:118`) | `EnableBracketedPaste` → `Event::Paste(String)` (whole paste). **Windows:** `EnableBracketedPaste` returns `Unsupported` (`crossterm/src/event.rs:424-432`), no `Event::Paste` exists there: a paste arrives as key events, Enter included. Needs a burst heuristic (many keys within a few ms = paste). |
| Kitty keyboard | push flags 7 (disambiguate, event types, alternate keys) + `\x1b[?u` + DA sentinel; pop `\x1b[<u` (`P/terminal.js:11-13,179-207`) | `supports_keyboard_enhancement()` (waits ≤2 s, returns at DA, `terminal/sys/unix.rs:243-262`), then `PushKeyboardEnhancementFlags`. Filter `KeyEventKind::Release` (and decide on `Repeat`). Windows emits Release events too. |
| modifyOtherKeys | fallback `\x1b[>4;2m` when DA arrives without kitty flags (`P/terminal.js:186-207,267-278`); pi-tui parses `\x1b[27;mod;code~` (`P/keys.js:514`) | crossterm 0.29 **rejects** CSI 27 (`event/sys/unix/parse.rs:641-653`). Do not enable modifyOtherKeys unless we parse input ourselves. Legacy terminals then lose shift+enter (alt+enter and `\`+enter remain) and ctrl+shift+f. |
| Escape timeout | lone ESC = Esc after 10 ms, 100 ms over SSH, `PI_TUI_ESC_TIMEOUT` (`P/terminal.js:53-70`) | crossterm has no knob; alt+key over slow SSH may split. |
| Native shift+enter | Apple Terminal and Windows: `\r` with shift held (CGEventSourceFlagsState) becomes `\x1b[13;2u` (`P/terminal.js:263-265`) — only if the native helper loads; it is looked up next to the executable for standalone builds (`P/native-module-path.js`), so likely inactive in the release binary | Windows: crossterm reads console records with modifiers, shift+enter works. macOS Terminal.app: optional FFI to `CGEventSourceFlagsState`. |
| Windows VT input | `ENABLE_VIRTUAL_TERMINAL_INPUT` via native helper for shift+tab (`P/terminal.js:285-295`) | Not needed: crossterm uses `ReadConsoleInputW` (BackTab works). |
| Clipboard | pbcopy (`LC_ALL=en_US.UTF-8`), `clip` (UTF-16LE with BOM), wl-copy → xclip → xsel; over SSH (`SSH_CONNECTION`/`SSH_TTY`) only OSC 52 `\x1b]52;c;<b64>\x07` (`clipboard.ts:7-30`). Used by drag-select, `/copy`, the pager's `y`. | Spawn the same tools (arboard on X11 loses the selection when the process exits); OSC 52 written between frames. No tmux passthrough wrapping in TS either. |
| Bell | `\x07` after a run ≥30 s that was not aborted, `config.tui.bell !== false` (`app.ts:47,559`) | Write between frames. |
| Resize | `stdout 'resize'` → render; size change → full redraw; components re-wrap (caches keyed by width) | `Event::Resize`; ratatui `autoresize` clears on change. |
| Progress, OSC 11, 996/2031, cell size | not used by sasacode | skip |

## 6. The local patch (`patches/@earendil-works%2Fpi-tui@0.87.1.patch`, rationale in `docs/dependencies.md:12`)

1. `setAmbiguousWidth(1|2)` in `utils.js` (exported from `index`): passes `ambiguousAsWide` to
   `get-east-asian-width` in `graphemeWidth` and clears the width cache. pi-tui always counted East Asian
   Ambiguous characters (①, ○) as 1, so lines overflowed on CJK-configured terminals (commit 8d437e6).
2. `Editor.expandPasteMarkers`: one regex pass; a marker is expanded only if it equals exactly what was
   inserted for that id (`pasteMarker(id, content)`), so a typed `[paste #1]` or a marker inside pasted text is
   sent as written (#25, commit fe6d618). Marker creation moved into the shared `pasteMarker()`.
3. `Editor.moveToEnd()`: cursor to the end of the text, so esc can append queued messages after the draft
   without `setText` (which forgets folded pastes) (#35).
4. `setAltScreenSearchText({placeholder, noMatches})`: the search box's two strings follow `/language` (#32).
5. Argument completion lists use `maxPrimaryColumnWidth: 64` instead of the default 32, so `/model` ids show
   whole as in the picker (commit 557acda).

All five are just behaviour of our own code in Rust: no patch mechanism needed.

## 7. How the TS tests drive the UI, and the Rust equivalent

TS (`app.test.ts:16-58`): a `FakeTerminal` implements pi-tui's `Terminal` (100×40, `kittyProtocolActive=false`),
records every byte written, and feeds input strings (`"\r"`, `"\x1b[Z"`, `"\x1b[102;6u"`, split
`"\x1b[200~…"`); `Bun.sleep(30–300)` lets the async app render; assertions read *everything written since an
offset* with escapes stripped (`term.text(from)`), sometimes raw SGR (`/\x1b\[31m(\x1b\[1m)?権限: 自動許可/`,
`app.test.ts:130`). A replay provider stands in for the model; `altScreen` is false except in the search tests.
Component tests (`views.test.ts`, `text-viewer.test.ts`, `activity.test.ts`) call `render(width)` and compare
plain lines, check every line's width (`Bun.stringWidth`), and inject `now`/`random` into `Activity`. The e2e
suite (`packages/e2e`) is headless only and does not drive the TUI.

Rust plan:
- Components keep `fn render(&mut self, width: u16) -> Vec<StyledLine>`; tests compare `plain()` of lines and
  `text::width` ≤ width, a 1:1 port of `views.test.ts`. SGR regexes become style checks on spans (fg red + bold).
- App tests drive `App` through a `TermIo` trait with a fake that takes **crossterm `Event`s**, not bytes
  (crossterm's parser is `pub(crate)`), so `"\x1b[102;6u"` becomes `Key(ctrl+shift+f)` and a split bracketed
  paste becomes one `Event::Paste`; keep a small byte→Event helper for the common sequences.
- Assert on the **composed frame**, which is mode-independent: before either renderer runs, the app produces
  `ComposedFrame { lines: Vec<StyledLine>, cursor: Option<(row, col)> }` (full screen: the visible rows; inline:
  the whole document). The fake **keeps every composed frame** (TS asserts on "was ever shown"): helpers
  `frames_since(n)`, `screen_text()`, `assert_shown(..)`. This covers the inline-mode tests, whose renderer does
  not go through a ratatui `Buffer`.
- Full-screen runs additionally render through `TestBackend` (cells, `ForcedWidth`, scroll, search, selection);
  `TestBackend::cursor_position()` / `cursor_visible()` check the IME cursor (moved, hidden). If some TS inline
  tests are moved to full screen instead, list them in `rust/MAP.md` as deviations.
- No sleeps: run the app on a current-thread tokio runtime with paused time (`tokio::time::pause`), advance the
  80 ms tick and the 16 ms render throttle explicitly, inject `Clock` and RNG into Activity.
- One byte-level suite: `CrosstermBackend<Vec<u8>>` → `vt100` 0.18 parser, to assert synchronized-output
  framing, cursor-hidden-but-moved, OSC 52, bell, OSC 8 cells and the restore sequence on exit.
- `insta` snapshots of whole screens are fine for layouts; keep exact-string assertions for the TS-quoted ones.

## 8. Suggested crate layout (`crates/sasacode-tui/src/`)

| Rust module | From | Contents |
| --- | --- | --- |
| `lib.rs` | `index.ts`, `app.ts:73-104` | `run_tui(host, initial_prompt, term)`, `TuiHost` |
| `app/mod.rs`, `app/input.rs`, `app/events.rs`, `app/commands.rs`, `app/draft.rs`, `app/history.rs` | `app.ts` | App state; key routing, paste, marker delete; agent events; core commands; held images and `[Image #n]`; history file |
| `term/mod.rs` | `ProcessTerminal`, `TuiBase` | `TermIo` trait (real/fake), event loop, render throttle, panic/exit restore |
| `term/setup.rs`, `term/probe.rs` | `P/terminal.js`, `P/tui-alt-screen.js:145-219`, `ambiguous.ts` | Enter/leave sequences, mouse modes, kitty push/pop; ① DSR probe |
| `term/frame.rs`, `term/inline.rs` | `P/tui-alt-screen.js:1431-1509`, `P/tui-main-screen.js` | Lines → `Buffer` with ForcedWidth, OSC 8, cursor; inline row-diff renderer |
| `ui/{component,container,text,padded,stack,scroll,select_list,input,search,selection,flash}.rs` | `Container`, `Text`, `Spacer`, `Padded`, `VStack`, `ScrollView`, `SelectList`, `Input`, alt-screen search/selection/flash | Component trait and building blocks |
| `text/{width,wrap,truncate,style,sanitize}.rs` | `P/utils.js`, `views.ts:18,202,244` | Width, `wrapTextWithAnsi`, `truncateToWidth`, `StyledLine`/span + link attr, `plain`/`oneLine`/`safeLine` |
| `editor/{mod,keys,paste,history,undo,kill_ring,word,autocomplete,fuzzy}.rs` | `P/components/editor.js`, `P/autocomplete.js`, `P/fuzzy.js`, `P/word-navigation.js` | Editor with patched behaviour |
| `keys.rs` | `P/keys.js`, `P/keybindings.js` | `KeyId` ("ctrl+shift+f") ↔ crossterm `KeyEvent`, default bindings + user overrides |
| `markdown/{mod,table,latex}.rs`, `links.rs` | `P/components/markdown.js`, `P/latex.js`, `links.ts` | Renderer; file links |
| `views.rs`, `dialogs.rs`, `activity.rs`, `text_viewer.rs`, `clipboard.rs`, `summary.rs`, `theme.rs` | same-named `.ts` | 1:1 ports |
| `testing.rs` (cfg test / feature) | `FakeTerminal` | Event-fed fake terminal with frame history |

Represent styled text as our own spans (`text`, `Style`, `link: Option<Arc<str>>`) rather than ANSI strings, and
convert to cells at the end; port `wrapTextWithAnsi`'s style carry-over as span logic.

## Traps for the Rust port

- [ ] Do not let ratatui measure sasacode text: write cells with our width and `CellDiffOption::ForcedWidth`;
      otherwise ambiguous=2 terminals and lone flags drift (the backend omits `MoveTo` for adjacent cells).
- [ ] Strip escapes/controls before measuring: unicode-width counts each control as 1 in a `str`, tab as 1
      (pi-tui: 0 and 3); ratatui debug-asserts on a lone control symbol.
- [ ] `width_cjk` ≠ `ambiguousAsWide` for Greek/Cyrillic/Latin-1 letters: use an EAW=A table or document it.
- [ ] Pin `unicode-width` exactly; ratatui follows the workspace version.
- [ ] CJK wraps per character, with style carry-over and `trimEnd`; ellipsis is `"..."`; a `Text` with only
      whitespace renders zero lines; `Padded` keeps ≥10 inner columns.
- [ ] Cursor: move the hardware cursor to the editor cursor every frame and keep it hidden
      (`Terminal::set_cursor_position` after a draw without a frame cursor); a frame cursor would show it.
- [ ] Order: enter 1049 *before* pushing kitty flags, pop them *before* leaving 1049 (separate stacks per screen).
- [ ] The editor wraps with its own `wordWrapLine` (keeps spaces, maps chunks to text offsets), not with the
      transcript's `wrapTextWithAnsi`.
- [ ] Filter `KeyEventKind::Release` (kitty flag 2, and always on Windows).
- [ ] Never enable modifyOtherKeys with crossterm's parser (CSI 27 is rejected); legacy terminals lose
      shift+enter and ctrl+shift+f, alt+enter and `\`+enter must keep working.
- [ ] Windows has no bracketed paste in crossterm: add a paste-burst heuristic or a multi-line paste submits.
- [ ] The ① probe needs a 400 ms timeout and must run before crossterm's reader; `cursor::position()` waits 2 s.
- [ ] Mouse mode 1003 must stay off under tmux/screen/zellij; crossterm's `EnableMouseCapture` turns it on.
- [ ] Wrap frame + cursor move in DEC 2026; restore everything (mouse, autowrap, 2004, kitty pop, 1049, cursor,
      raw mode) on exit, on panic (hook) and on error paths; TS has no such guard, Rust must.
- [ ] Exit in full screen leaves the alt screen without replaying the transcript, then prints `\r\x1b[2K` +
      summary + `sasacode -r <id>` on the main screen.
- [ ] The top margin line scrolls with the transcript; the bottom one is fixed under the footer.
- [ ] Key order: viewport keys (pageup/down, ctrl+↑↓, ctrl+home/end, search) → app keys → focused component;
      while the pager or search has focus the app must not eat esc/paste (`app.ts:278,308`).
- [ ] Paste thresholds: ≤40 lines and ≤4,000 chars inline; else editor folds (>10 lines → `+L lines`, else
      `C chars`); expand only exact markers; trim on submit; `setText` drops folded pastes.
- [ ] `[Image #n]` and paste markers are atomic for backspace/delete and cursor moves; byte offsets on
      grapheme boundaries, not UTF-16 indices.
- [ ] History: 100 in memory, last 200 read from file, newest first, no consecutive duplicates, file 0600.
- [ ] Enter on a slash-command completion submits; on an argument completion it only completes; exact
      argument matches return no items (#114).
- [ ] `@` mentions stay off (no fd); Tab completes paths from the cwd.
- [ ] Japanese word motion differs between ICU and UAX #29: `icu_segmenter` or a recorded deviation.
- [ ] LaTeX in markdown is on in TS: port `latex.js` or record the drop.
- [ ] Link targets: cut `#…` before `decodeURIComponent`, `%23` stays a `#` in the name, drop `:line`, keep
      `tel:`-like URLs, emit OSC 8 only on known terminals, else `text (url)`.
- [ ] Sanitize everything from the model/tools with `plain()` (ESC → `␛`), tool headers with `oneLine()`,
      approval commands with the control-character warning.
- [ ] Keep per-entry line caches keyed by width and expanded state; re-rendering the full transcript every
      80 ms tick is otherwise too slow (110 ms/frame at 50,000 lines in TS before the cache).
- [ ] The selection copies from the scroll content, not the screen; "Copied!" is English in TS.
- [ ] Clipboard: native tool first (pbcopy with UTF-8 locale, clip with UTF-16LE BOM), OSC 52 only over SSH or
      as fallback.
- [ ] Tests feed `Event`s and keep every frame; no sleeps (paused tokio time, injected clock).
