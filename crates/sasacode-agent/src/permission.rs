//! The permission policy (`packages/agent/src/permission.ts`): rules, modes, and path matching
//! that symlinks, case folding and Unicode normalisation cannot get around. This is a security
//! boundary: every regression test of the TS version is ported below.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock, RwLock};

use regex::Regex;
use unicode_normalization::UnicodeNormalization;

use sasacode_ai::JsonObject;
use sasacode_plugin_api::{Decision, PermissionMode, PermissionRules, ToolKind, ToolRef, Verdict, VerdictSource};

use crate::paths::{self, WINDOWS};

pub const PERMISSION_MODES: [PermissionMode; 4] = [
    PermissionMode::Edits,
    PermissionMode::Ask,
    PermissionMode::Agent,
    PermissionMode::Auto,
];

pub fn permission_mode_label(m: PermissionMode) -> &'static str {
    match m {
        PermissionMode::Edits => "読み取り・編集のみ許可",
        PermissionMode::Ask => "ユーザー確認",
        PermissionMode::Agent => "エージェント判断",
        PermissionMode::Auto => "自動許可",
    }
}

#[derive(Clone)]
pub struct PermissionCheck {
    pub tool: ToolRef,
    pub args: JsonObject,
    pub cwd: String,
}

#[derive(Debug, Clone)]
struct ParsedRule {
    tool: Regex,
    pattern: Option<String>,
    source: String,
    /// An allow rule's absolute pattern and its links as resolved when it was added.
    real: Option<(String, String)>,
}

#[derive(Debug, Default)]
struct Rules {
    allow: Vec<ParsedRule>,
    ask: Vec<ParsedRule>,
    deny: Vec<ParsedRule>,
    soft_deny: Vec<ParsedRule>,
}

pub struct PermissionPolicy {
    mode: RwLock<PermissionMode>,
    rules: RwLock<Rules>,
}

impl Default for PermissionPolicy {
    fn default() -> Self {
        PermissionPolicy {
            mode: RwLock::new(PermissionMode::Edits),
            rules: RwLock::new(Rules::default()),
        }
    }
}

impl PermissionPolicy {
    /// Errs on a rule that does not parse ("invalid permission rule: …"), as the TS constructor threw.
    pub fn new(mode: PermissionMode, rules: &PermissionRules) -> anyhow::Result<Self> {
        let p = PermissionPolicy {
            mode: RwLock::new(mode),
            rules: RwLock::new(Rules::default()),
        };
        p.add_rules(rules)?;
        Ok(p)
    }

    pub fn mode(&self) -> PermissionMode {
        *self.mode.read().unwrap_or_else(|e| e.into_inner())
    }

    pub fn set_mode(&self, mode: PermissionMode) {
        *self.mode.write().unwrap_or_else(|e| e.into_inner()) = mode;
    }

    /// Adds rules in the order allow, ask, deny, softDeny; stops at the first that does not parse,
    /// keeping those before it (as the TS did).
    pub fn add_rules(&self, rules: &PermissionRules) -> anyhow::Result<()> {
        let mut r = self.rules.write().unwrap_or_else(|e| e.into_inner());
        let lists: [(&Vec<String>, u8); 4] = [
            (&rules.allow, 0),
            (&rules.ask, 1),
            (&rules.deny, 2),
            (&rules.soft_deny, 3),
        ];
        for (list, which) in lists {
            for s in list {
                let mut parsed = parse_rule(s).ok_or_else(|| anyhow::anyhow!("invalid permission rule: {s}"))?;
                // An allow rule follows the links there now; one made later, or the rule added again, does not
                // take it elsewhere.
                if which == 0 && r.allow.iter().any(|a| a.source == *s) {
                    continue;
                }
                let abs = parsed.pattern.as_deref().map(|p| path_pattern(p, "."));
                if let Some(p) = abs.filter(|p| which == 0 && paths::is_absolute(p)) {
                    parsed.real = Some((p.clone(), real_pattern(&p)));
                }
                match which {
                    0 => r.allow.push(parsed),
                    1 => r.ask.push(parsed),
                    2 => r.deny.push(parsed),
                    _ => r.soft_deny.push(parsed),
                }
            }
        }
        Ok(())
    }

    pub fn check(&self, c: &PermissionCheck) -> Verdict {
        let rules = self.rules.read().unwrap_or_else(|e| e.into_inner());
        let v = |decision, reason: String, source| Verdict {
            decision,
            reason,
            source,
        };
        if let Some(d) = rules.deny.iter().find(|r| rule_matches(r, c, false)) {
            return v(Decision::Deny, format!("rule deny: {}", d.source), VerdictSource::Rule);
        }
        if let Some(s) = rules.soft_deny.iter().find(|r| rule_matches(r, c, false)) {
            return v(
                Decision::Deny,
                format!("rule softDeny: {}", s.source),
                VerdictSource::Soft,
            );
        }
        if let Some(a) = rules.ask.iter().find(|r| rule_matches(r, c, false)) {
            return v(Decision::Ask, format!("rule ask: {}", a.source), VerdictSource::Rule);
        }
        // Where a path goes cannot be told (a link loop), or differs opened as given (a ".." after a link): ask.
        let cwd = std::path::Path::new(&c.cwd);
        if let Some(unclear) = c.tool.paths(&c.args, cwd).unwrap_or_default().into_iter().find(|p| {
            let real = try_real_path(&paths::resolve2(&c.cwd, p));
            real.is_none() || try_real_path(&cwd.join(p).to_string_lossy()).is_some_and(|os| Some(os) != real)
        }) {
            return v(Decision::Ask, format!("cannot resolve {unclear}"), VerdictSource::Rule);
        }
        if allowed_by_rules(&rules.allow, c) {
            return v(Decision::Allow, "allow rule".into(), VerdictSource::Rule);
        }
        drop(rules);
        let (decision, reason) = self.mode_decision(c);
        v(decision, reason.into(), VerdictSource::Mode)
    }

    /// What the deny, softDeny and ask rules alone say about this call (no mode, no allow rules).
    pub fn rule_decision(&self, c: &PermissionCheck) -> Option<Decision> {
        let rules = self.rules.read().unwrap_or_else(|e| e.into_inner());
        let hit = |list: &[ParsedRule]| list.iter().any(|r| rule_matches(r, c, false));
        if hit(&rules.deny) || hit(&rules.soft_deny) {
            return Some(Decision::Deny);
        }
        hit(&rules.ask).then_some(Decision::Ask)
    }

    fn mode_decision(&self, c: &PermissionCheck) -> (Decision, &'static str) {
        // Through a symlink, a path inside the project can point anywhere: judge where it really goes.
        let cwd = try_real_path(&c.cwd).unwrap_or_else(|| c.cwd.clone());
        let real: Vec<String> = paths_of(c)
            .iter()
            .flat_map(|p| spellings(p, true))
            .map(|p| try_real_path(&p).unwrap_or(p))
            .collect();
        let inside_cwd = real.iter().all(|p| is_inside(p, &cwd));
        let has_paths = c.tool.paths(&c.args, std::path::Path::new(&c.cwd)).is_some();
        let runs_code = real.iter().any(|p| runs_code(&cwd, p));
        let kind = c.tool.kind();
        match self.mode() {
            PermissionMode::Auto => (Decision::Allow, "mode auto"),
            PermissionMode::Ask => (Decision::Ask, "mode ask"),
            PermissionMode::Edits => {
                if (kind == ToolKind::Read || (kind == ToolKind::Edit && !runs_code)) && has_paths && inside_cwd {
                    (Decision::Allow, "mode edits: inside working directory")
                } else {
                    (Decision::Ask, "mode edits")
                }
            }
            PermissionMode::Agent => {
                if kind == ToolKind::Read && has_paths && inside_cwd {
                    (Decision::Allow, "read inside working directory")
                } else {
                    // The model's judgement is a permission hook (the bundled agent-judge plugin).
                    (Decision::Ask, "mode agent: not judged")
                }
            }
        }
    }

    /// Rules that "always allow" in the approval dialog adds for this call: no wider than what was
    /// shown. A command is matched as written (its `*` escaped, so they are not wildcards); a file
    /// tool gets one rule per path it touches; only a tool with neither is allowed by name.
    pub fn rules_for(c: &PermissionCheck) -> Vec<String> {
        let name = c.tool.name();
        if let Some(target) = c.tool.match_target(&c.args) {
            return vec![format!("{name}({})", target.replace('\\', "\\\\").replace('*', "\\*"))];
        }
        let paths = paths_of(c);
        if !paths.is_empty() {
            let mut seen = Vec::<String>::new();
            for p in paths {
                if !seen.contains(&p) {
                    seen.push(p);
                }
            }
            return seen
                .iter()
                .map(|p| format!("{name}({})", escape_glob(&slashes(p))))
                .collect();
        }
        vec![name.to_string()]
    }

    /// rules_for, for display.
    pub fn rule_for(c: &PermissionCheck) -> String {
        Self::rules_for(c).join(", ")
    }
}

fn runs_code_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    // INFALLIBLE: a constant pattern.
    R.get_or_init(|| Regex::new(r"(?i)(^|[\\/])\.(git|sasacode)([\\/]|$)").expect("valid regex"))
}

/// Edits under .git (hooks, config) or .sasacode (plugins, rules) run code at the next git command or start (#127).
/// Git takes a folder with a HEAD and a config, commondir, objects/ or refs/ for a repository: a write that could make
/// or point one, or change its hooks, is asked about. Names are compared folded in full (`ﬁ` as `fi`), as a file
/// system that folds case takes them.
fn runs_code(cwd: &str, p: &str) -> bool {
    use std::path::Path;
    let path = Path::new(p);
    let name = |q: &Path| full_fold(&q.file_name().unwrap_or_default().to_string_lossy());
    let has = |d: &Path, n: &str| d.join(n).metadata().is_ok_and(|m| !m.is_dir());
    let repo = |d: &Path| {
        has(d, "HEAD")
            && ["config", "commondir", "objects", "refs"]
                .iter()
                .any(|n| d.join(n).exists())
    };
    let dir = path.parent().unwrap_or(path);
    let in_store = |a: &Path| matches!(&*name(a), "objects" | "refs" | "hooks") && a.parent().is_some_and(repo);
    match &*name(path) {
        "head" | "commondir" | "gitdir" => true,
        "config" | "config.worktree" if has(dir, "HEAD") || has(dir, "commondir") => true,
        "packed-refs" if repo(dir) => true,
        _ => path.ancestors().skip(1).any(in_store) || runs_code_re().is_match(&full_fold(&paths::relative(cwd, p))),
    }
}

/// Escape `\ * ? [ ] { } ! ( ) + @` with a backslash (`/[\\*?[\]{}!()+@]/g` → `\\$&`).
fn escape_glob(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        if matches!(
            ch,
            '\\' | '*' | '?' | '[' | ']' | '{' | '}' | '!' | '(' | ')' | '+' | '@'
        ) {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// A rule written for this tool, or for the tool whose rules it takes on (permissionsAs).
fn covers(rule: &ParsedRule, tool: &ToolRef) -> bool {
    rule.tool.is_match(tool.name()) || tool.permissions_as().is_some_and(|p| rule.tool.is_match(&p))
}

/// Rules look like `bash`, `bash(git status*)`, `edit(src/**)`, `mcp__github__*`.
fn parse_rule(source: &str) -> Option<ParsedRule> {
    static R: OnceLock<Regex> = OnceLock::new();
    // INFALLIBLE: a constant pattern. Lazy `+?` on the name, as in the TS.
    let re = R.get_or_init(|| Regex::new(r"(?s)^([^()]+?)(?:\((.*)\))?$").expect("valid regex"));
    let m = re.captures(source.trim())?;
    let tool = wildcard(m.get(1)?.as_str())?;
    Some(ParsedRule {
        tool,
        pattern: m.get(2).map(|p| p.as_str().to_string()),
        source: source.to_string(),
        real: None,
    })
}

/// `*` matches anything; `\*` is a literal `*` and `\\` a literal backslash.
fn wildcard(p: &str) -> Option<Regex> {
    let chars: Vec<char> = p.chars().collect();
    let mut re = String::from("(?s)^");
    let mut i = 0;
    while i < chars.len() {
        let mut ch = chars[i];
        if ch == '\\' && matches!(chars.get(i + 1), Some('*') | Some('\\')) {
            i += 1;
            ch = chars[i];
        } else if ch == '*' {
            re.push_str(".*");
            i += 1;
            continue;
        }
        re.push_str(&regex::escape(&ch.to_string()));
        i += 1;
    }
    re.push('$');
    Regex::new(&re).ok()
}

/// The paths a call touches, absolute: a plugin returning relative ones must not slip past path rules.
fn paths_of(c: &PermissionCheck) -> Vec<String> {
    c.tool
        .paths(&c.args, std::path::Path::new(&c.cwd))
        .unwrap_or_default()
        .iter()
        .map(|p| paths::resolve2(&c.cwd, p))
        .collect()
}

/// Shell operators that chain commands; an allow rule must match every piece.
pub fn chain_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    // INFALLIBLE: a constant pattern.
    R.get_or_init(|| Regex::new(r"&&|\|\||;|\||\n|&").expect("valid regex"))
}

/// Substitutions and output redirection can do anything, so allow rules never cover them.
fn substitution_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    // INFALLIBLE: a constant pattern.
    R.get_or_init(|| Regex::new(r"\$\(|`|<\(|>").expect("valid regex"))
}

/// `(?<!\\)[+@](?=\()` → prefixed with a backslash (no lookaround in `regex`: done by hand).
fn escape_extglob(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    for (i, &ch) in chars.iter().enumerate() {
        if (ch == '+' || ch == '@') && chars.get(i + 1) == Some(&'(') && (i == 0 || chars[i - 1] != '\\') {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// A compiled glob as Bun.Glob matched it: `*` stops at `/`, `**` crosses it, `\` escapes. Kept: a walk asks per file.
/// Folded (glob_path) outside `[…]` after its backslashes are read, so `\B` is a `B` folded, not `\b`.
fn glob(pattern: &str, fold: Option<bool>) -> regex::bytes::Regex {
    static SEEN: OnceLock<Mutex<HashMap<String, regex::bytes::Regex>>> = OnceLock::new();
    let seen = SEEN.get_or_init(Mutex::default);
    let mut seen = seen.lock().unwrap_or_else(|e| e.into_inner());
    if seen.len() > 512 {
        seen.clear();
    }
    let g = seen
        .entry(format!("{fold:?}{pattern}"))
        .or_insert_with(|| compile_glob(pattern, fold));
    g.clone()
}

fn compile_glob(pattern: &str, fold: Option<bool>) -> regex::bytes::Regex {
    let built = globset::GlobBuilder::new(&bun_glob_syntax(pattern, fold))
        .literal_separator(true)
        .backslash_escape(true)
        .empty_alternates(true)
        .build();
    match built {
        Ok(g) => char_regex(&g, fold),
        // An unparsable pattern (an unclosed `[`) is taken literally, never as matching nothing:
        // a deny rule must not disappear.
        Err(_) => globset::GlobBuilder::new(&escape_glob(&folded(bun_glob_syntax(pattern, None), fold)))
            .literal_separator(true)
            .backslash_escape(true)
            .build()
            .map(|g| char_regex(&g, None))
            // INFALLIBLE: every special character is escaped.
            .unwrap_or_else(|_| char_regex(&globset::Glob::new("").expect("empty glob"), None)),
    }
}

/// globset's regex reads bytes (its `?` and `[!a]` take one byte of `ß`); Bun.Glob read characters. The same regex
/// with its bytes of a character (`\xc3\x9f`) put back as that character, read over characters; `.` takes a newline
/// too (a name may hold one). Folded in full, a class `[…]` matches any case; in lower case (an allow rule's) no
/// wider than its letters lowered: an ASCII class in ASCII case (`ſ` is not `s` to it), another as written.
fn char_regex(g: &globset::Glob, fold: Option<bool>) -> regex::bytes::Regex {
    let (mut out, mut bytes, mut it) = (String::new(), Vec::new(), g.regex().trim_start_matches("(?-u)").chars());
    let mut start = 0;
    while let Some(ch) = it.next() {
        let esc = if ch == '\\' { it.next() } else { None };
        let hex: String = if esc == Some('x') {
            it.by_ref().take(2).collect()
        } else {
            String::new()
        };
        if let Ok(b @ 0x80..) = u8::from_str_radix(&hex, 16) {
            bytes.push(b);
            continue;
        }
        out.push_str(&String::from_utf8_lossy(&std::mem::take(&mut bytes)));
        match (ch, esc) {
            ('[', None) if fold.is_some() => start = out.len(),
            (']', None) if fold.is_some() => {
                let class = format!("[{}]", out.split_off(start));
                let any = (fold == Some(true) || class.starts_with("[^")).then_some("i");
                let flags = any.or(class.is_ascii().then_some("i-u")).unwrap_or_default();
                out.push_str(&format!("(?{flags}:{class})"));
            }
            _ => out.extend([ch].into_iter().chain(esc).chain(hex.chars())),
        }
    }
    out.push_str(&String::from_utf8_lossy(&bytes));
    let re = |r: &str| regex::bytes::RegexBuilder::new(r).dot_matches_new_line(true).build();
    // INFALLIBLE in practice: globset's own regex, read over bytes, is the fallback.
    re(&out).or_else(|_| re(g.regex())).expect("globset's regex")
}

/// Bun.Glob's reading where globset's differs. A `}` that closes no `{` is a literal brace
/// (`\{a,b}` matched the name `{a,b}`; globset refuses the pattern), and `\b` `\n` `\r` `\t`
/// are the control characters, as in a JS string (`a\b` did not match `ab`). Folded (`fold`) outside `[…]`.
fn bun_glob_syntax(pattern: &str, fold: Option<bool>) -> String {
    let (mut out, mut run) = (String::with_capacity(pattern.len()), String::new());
    let (mut depth, mut in_class, mut chars) = (0usize, false, pattern.chars().peekable());
    while let Some(ch) = chars.next() {
        if ch == '[' && !in_class {
            out.push_str(&folded(std::mem::take(&mut run), fold));
        }
        let out = if in_class || ch == '[' { &mut out } else { &mut run };
        match ch {
            '\\' => {
                match chars.next() {
                    Some('b') => out.push('\u{8}'),
                    Some('n') => out.push('\n'),
                    Some('r') => out.push('\r'),
                    Some('t') => out.push('\t'),
                    Some(next) => {
                        in_class &= next != ']'; // globset's class ends at `\]` too
                        out.push(ch);
                        out.push(next);
                    }
                    None => out.push(ch),
                }
                continue;
            }
            '[' if !in_class => {
                in_class = true;
                out.push(ch);
                // A `]` right after the opening (or after `!`/`^`) belongs to the class.
                if let Some(&c) = chars.peek()
                    && (c == '!' || c == '^')
                {
                    out.push(c);
                    chars.next();
                }
                if chars.peek() == Some(&']') {
                    out.push(']');
                    chars.next();
                }
                continue;
            }
            ']' if in_class => in_class = false,
            '{' if !in_class => depth += 1,
            '}' if !in_class && depth == 0 => {
                out.push_str("\\}");
                continue;
            }
            '}' if !in_class => depth -= 1,
            _ => {}
        }
        out.push(ch);
    }
    out.push_str(&folded(run, fold));
    folded(out, None)
}

/// deny/ask rules match when any spelling of a path does (as written, or with symlinks resolved);
/// allow rules only when every spelling does, so a symlink cannot smuggle a path past either.
fn rule_matches(rule: &ParsedRule, c: &PermissionCheck, for_allow: bool) -> bool {
    if !covers(rule, &c.tool) {
        return false;
    }
    let Some(pattern) = &rule.pattern else { return true };
    if let Some(target) = c.tool.match_target(&c.args) {
        let Some(re) = wildcard(pattern) else { return false };
        return re.is_match(&target) || segments(&target).iter().any(|s| re.is_match(s));
    }
    let paths = paths_of(c);
    if paths.is_empty() {
        return false;
    }
    // Compare like with like. A relative pattern is anchored at the working directory's real path,
    // but links inside the project are not followed (a repo could point `src` at ~/.ssh). An
    // absolute pattern is the user's own path: its fixed part is resolved (/var → /private/var).
    let pattern = path_pattern(pattern, &c.cwd);
    // .ENV is .env where the file system folds case (#98). Whether a path's volume does cannot be told reliably, so
    // deny and ask rules hit as written, lowered or folded in full (`ß` is `ss`; folded alone, `[!a-z]x` misses `Sx`).
    // Allow rules fold to lower case only where the working directory does.
    let real_cwd = try_real_path(&c.cwd).unwrap_or_else(|| c.cwd.clone());
    let spellings: Vec<String> = paths
        .iter()
        .flat_map(|p| spellings(p, !for_allow))
        .flat_map(|p| [try_real_path(&p).unwrap_or_else(|| p.clone()), p])
        .collect();
    let mut sources: Vec<String> = if paths::is_absolute(&pattern) {
        // An allow rule's links as they were when it was added; deny and ask rules' as they are.
        let real = match &rule.real {
            _ if !for_allow => real_pattern(&pattern),
            Some((p, real)) if *p == pattern => real.clone(),
            _ => pattern.clone(),
        };
        [pattern, real]
            .iter()
            .map(|g| escape_extglob(&glob_path(g, None)))
            .collect()
    } else {
        [&c.cwd, &real_cwd].iter().map(|d| anchored(d, &pattern)).collect()
    };
    sources.dedup();
    let hit = |fold: Option<bool>| {
        let globs: Vec<regex::bytes::Regex> = sources.iter().map(|g| glob(g, fold)).collect();
        move |p: &String| globs.iter().any(|g| g.is_match(glob_path(p, fold).as_bytes()))
    };
    if for_allow {
        return spellings.iter().all(hit(folds_case(&real_cwd).then_some(false)));
    }
    [None, Some(false), Some(true)]
        .into_iter()
        .any(|f| spellings.iter().any(hit(f)))
}

/// A path as written and, on Windows, as Win32 opens it (`.git.` is `.git`; with `streams`, `a:s` is `a` too).
fn spellings(p: &str, streams: bool) -> Vec<String> {
    let mut v = vec![p.to_string()];
    let open = |s| paths::resolve(&[&sasacode_plugin_api::win32_path::win32_open_form(p, s)]);
    if WINDOWS {
        v.extend([open(false), open(true)].into_iter().take(1 + streams as usize));
    }
    v
}

/// On Windows, `\` as `/`: rules are globs, where `\` escapes (and no Windows name holds `*` or `?`).
fn slashes(p: &str) -> String {
    if WINDOWS { p.replace('\\', "/") } else { p.to_string() }
}

/// A path rule's pattern; on Windows `/x` (no drive) is on the working directory's drive.
fn path_pattern(pattern: &str, cwd: &str) -> String {
    if !WINDOWS {
        return pattern.to_string();
    }
    let p = slashes(pattern);
    if p.starts_with('/') && !p.starts_with("//") {
        let r = paths::resolve(&[cwd]);
        return format!("{}{p}", &r[..2.min(r.len())]);
    }
    p
}

/// A relative pattern under `dir`, which is a literal path: a `[`, `{` or `+(` in it is not a wildcard.
fn anchored(dir: &str, pattern: &str) -> String {
    // Normalized as resolve(dir, pattern) would, a ".." anywhere included (#50).
    let mut dir = dir.to_string();
    let mut rest: Vec<&str> = Vec::new();
    let pat = slashes(pattern);
    for part in pat.split('/') {
        if part.is_empty() || part == "." {
            continue;
        }
        if part != ".." {
            rest.push(part);
        } else if !rest.is_empty() {
            rest.pop();
        } else {
            dir = paths::dirname(&paths::resolve(&[&dir]));
        }
    }
    // resolve: on Windows a cwd given as /work/x gets its drive, as the paths compared with it do.
    // ( ) + @ too: Bun.Glob read +(…) and @(…) as extglobs (#98, #131).
    let base = escape_glob(&glob_path(&paths::resolve(&[&dir]), None));
    let base = base.strip_suffix('/').unwrap_or(&base).to_string();
    if rest.is_empty() {
        if base.is_empty() { "/".into() } else { base }
    } else {
        format!("{base}/{}", escape_extglob(&glob_path(&rest.join("/"), None)))
    }
}

/// A path or glob as compared, as the file system sees names: on Windows with `/`; NFC (macOS takes NFD as the
/// same); folded (`Some`) to lower case, or for deny and ask rules in full (full_fold).
fn glob_path(p: &str, fold: Option<bool>) -> String {
    folded(slashes(p), fold)
}

fn folded(s: String, fold: Option<bool>) -> String {
    let s = match fold {
        Some(true) => full_fold(&s),
        Some(false) => s.to_lowercase(),
        None => s,
    };
    if s.is_ascii() { s } else { s.nfc().collect() }
}

/// Each letter as its lower case's upper case's lower case, which may be longer: `ſ` is `s`, `ẞ` and `ß` are `ss`.
fn full_fold(s: &str) -> String {
    s.to_lowercase()
        .to_uppercase()
        .chars()
        .flat_map(char::to_lowercase)
        .collect()
}

/// Does the file system at `dir` take a name in another case for the same file (Windows; macOS's
/// APFS and HFS+ by default)?
fn folds_case(dir: &str) -> bool {
    if WINDOWS {
        return true;
    }
    static FOLDS: OnceLock<Mutex<HashMap<String, bool>>> = OnceLock::new();
    let m = FOLDS.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(v) = m.lock().unwrap_or_else(|e| e.into_inner()).get(dir) {
        return *v;
    }
    let other: String = dir
        .chars()
        .map(|ch| {
            if ch.is_alphabetic() {
                let lower: String = ch.to_lowercase().collect();
                if lower == ch.to_string() {
                    ch.to_uppercase().collect::<String>()
                } else {
                    lower
                }
            } else {
                ch.to_string()
            }
        })
        .collect();
    let v = same_file(dir, &other) && other != dir;
    let mut m = m.lock().unwrap_or_else(|e| e.into_inner());
    if m.len() > 512 {
        m.clear();
    }
    m.insert(dir.to_string(), v);
    v
}

#[cfg(unix)]
fn same_file(a: &str, b: &str) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (std::fs::metadata(a), std::fs::metadata(b)) {
        (Ok(x), Ok(y)) => x.ino() == y.ino() && x.dev() == y.dev(),
        _ => false,
    }
}

#[cfg(not(unix))]
fn same_file(_a: &str, _b: &str) -> bool {
    false
}

/// The root `p` starts from: on Windows a drive (C:\) or share; a bare `\` is the current drive's.
fn root_of(p: &str) -> String {
    let root = paths::root_of_parse(p);
    if !WINDOWS {
        return if root.is_empty() { "/".into() } else { root };
    }
    let b: Vec<char> = root.chars().collect();
    if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == ':' {
        return format!("{}\\", &root[..2]);
    }
    if b.len() >= 2 && paths::is_sep(b[0]) && paths::is_sep(b[1]) {
        return root;
    }
    paths::resolve(&["\\"]).chars().take(3).collect()
}

/// A glob with the directories before its first wildcard resolved through symlinks.
fn real_pattern(glob: &str) -> String {
    let parts: Vec<&str> = glob.split('/').collect();
    let first_wild = parts.iter().position(|s| s.contains(['*', '?', '[', '{']));
    match first_wild {
        None | Some(0) => try_real_path(glob).unwrap_or_else(|| glob.to_string()),
        Some(i) => {
            let fixed = parts[..i].join("/");
            let fixed = if fixed.is_empty() { "/".to_string() } else { fixed };
            let real = try_real_path(&fixed).unwrap_or(fixed);
            std::iter::once(real.as_str())
                .chain(parts[i..].iter().copied())
                .collect::<Vec<_>>()
                .join("/")
        }
    }
}

/// `p` with symlinks resolved, walked one component at a time as the OS does: a link's target is
/// put in front of the rest of the path before any ".." that follows is applied. Parts that do not
/// exist yet are kept as written, and so is a link whose target does not exist yet, followed to that
/// target (writing through it would create it). Errs when the path cannot be resolved safely (a
/// link loop, no permission, ".." below a part that does not exist).
pub fn real_path(p: &str) -> Result<String, String> {
    let root_len = paths::root_of_parse(p).len();
    let mut todo: std::collections::VecDeque<String> = p[root_len.min(p.len())..]
        .split(paths::is_sep)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    let mut done = root_of(p);
    let mut absent = false;
    let mut hops = 0;
    while let Some(part) = todo.pop_front() {
        if part == "." {
            continue;
        }
        if part == ".." {
            // The OS would fail here, but the folder may be created in between: do not guess.
            if absent {
                return Err(format!("cannot resolve {p}: \"..\" below a path that does not exist"));
            }
            done = paths::dirname(&done);
            continue;
        }
        let next = paths::join(&done, &part);
        if !absent {
            let mut link: Option<String> = None;
            match std::fs::symlink_metadata(&next) {
                Ok(st) => {
                    if st.file_type().is_symlink() {
                        link = Some(
                            std::fs::read_link(&next)
                                .map_err(|e| e.to_string())?
                                .to_string_lossy()
                                .into_owned(),
                        );
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => absent = true,
                // Only ENOENT means "not there yet"; anything else (ENOTDIR, EACCES) is not guessed past.
                Err(e) => return Err(e.to_string()),
            }
            if let Some(link) = link {
                hops += 1;
                if hops > 40 {
                    return Err(format!("too many symlinks: {p}"));
                }
                let lroot = paths::root_of_parse(&link).len();
                let parts: Vec<String> = link[lroot.min(link.len())..]
                    .split(paths::is_sep)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect();
                for part in parts.into_iter().rev() {
                    todo.push_front(part);
                }
                if paths::is_absolute(&link) {
                    done = root_of(&link);
                }
                continue;
            }
        }
        done = next;
    }
    Ok(done)
}

pub fn try_real_path(p: &str) -> Option<String> {
    real_path(p).ok()
}

fn segments(command: &str) -> Vec<String> {
    chain_re()
        .split(command)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// For commands, every chained piece must be covered by some allow rule, and substitutions never are.
fn allowed_by_rules(rules: &[ParsedRule], c: &PermissionCheck) -> bool {
    let mine: Vec<&ParsedRule> = rules.iter().filter(|r| covers(r, &c.tool)).collect();
    if mine.is_empty() {
        return false;
    }
    if mine.iter().any(|r| r.pattern.is_none()) {
        return true;
    }
    let Some(target) = c.tool.match_target(&c.args) else {
        return mine.iter().any(|r| rule_matches(r, c, true));
    };
    // A rule without a wildcard ("always allow" adds one for the command shown) covers it whole,
    // chained or not (#100).
    let literal = |r: &ParsedRule| !strip_escapes(r.pattern.as_deref().unwrap_or("")).contains('*');
    let matches = |r: &ParsedRule, s: &str| r.pattern.as_deref().and_then(wildcard).is_some_and(|re| re.is_match(s));
    if mine.iter().any(|r| literal(r) && matches(r, &target)) {
        return true;
    }
    if substitution_re().is_match(&target) {
        return false;
    }
    segments(&target).iter().all(|s| mine.iter().any(|r| matches(r, s)))
}

/// `p.replace(/\\[\\*]/g, "")`: drop each `\\` and `\*`, left to right in one pass.
fn strip_escapes(p: &str) -> String {
    let mut out = String::with_capacity(p.len());
    let mut it = p.chars().peekable();
    while let Some(ch) = it.next() {
        if ch == '\\' && matches!(it.peek(), Some('\\') | Some('*')) {
            it.next();
            continue;
        }
        out.push(ch);
    }
    out
}

pub fn is_inside(path: &str, dir: &str) -> bool {
    let rel = paths::relative(dir, path);
    // "..foo" is a name inside; only ".." itself or "../…" goes up.
    rel.is_empty() || (rel != ".." && !rel.starts_with(&format!("..{}", paths::sep())) && !paths::is_absolute(&rel))
}

#[cfg(test)]
#[path = "permission_tests.rs"]
mod tests;
