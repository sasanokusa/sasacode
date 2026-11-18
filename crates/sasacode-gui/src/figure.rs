//! What a reply draws rather than writes: math, and Mermaid diagrams.
//!
//! The Markdown a reply is parsed from has its display math (`$$ … $$`, `\[ … \]`) made into
//! fenced blocks of the language `math`, which the reply draws as the code blocks of `mermaid` are:
//! as a picture, read from the SVG it is laid out to. Math inside a line is written in Unicode,
//! as far as it goes: a line of text holds no pictures. Code, fenced or inline, is left as it is.

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use iced::widget::svg;

/// The languages a fenced block of math is written as.
pub const MATH: [&str; 4] = ["math", "latex", "tex", "katex"];

/// How many pictures are kept made, until they are made again.
const KEPT: usize = 96;

/// A picture made, and the size it was laid out at.
#[derive(Debug, Clone)]
pub struct Figure {
    pub handle: svg::Handle,
    pub width: f32,
    pub height: f32,
}

type Made = HashMap<(bool, String), Result<Figure, String>>;

fn made() -> &'static Mutex<Made> {
    static MADE: OnceLock<Mutex<Made>> = OnceLock::new();
    MADE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The picture of `source`, made once: math when `math`, else a Mermaid diagram. A renderer that
/// panics on what it is given says so as its error.
pub fn figure(math: bool, source: &str) -> Result<Figure, String> {
    let key = (math, source.to_owned());
    if let Some(done) = made().lock().ok().and_then(|m| m.get(&key).cloned()) {
        return done;
    }
    let source = source.to_owned();
    let svg = std::panic::catch_unwind(move || if math { math_svg(&source) } else { mermaid_svg(&source) })
        .unwrap_or_else(|_| Err("the renderer stopped".to_owned()));
    let done = svg.and_then(|svg| {
        let (width, height) = size_of(&svg).ok_or_else(|| "no size".to_owned())?;
        Ok(Figure {
            handle: svg::Handle::from_memory(svg.into_bytes()),
            width,
            height,
        })
    });
    if let Ok(mut m) = made().lock() {
        if m.len() >= KEPT {
            m.clear();
        }
        m.insert(key, done.clone());
    }
    done
}

fn math_svg(tex: &str) -> Result<String, String> {
    let nodes = ratex_parser::parser::parse(tex.trim()).map_err(|e| e.to_string())?;
    let laid = ratex_layout::layout(&nodes, &ratex_layout::LayoutOptions::default());
    let list = ratex_layout::to_display_list(&laid);
    let options = ratex_svg::SvgOptions {
        embed_glyphs: true,
        font_size: 17.0,
        padding: 2.0,
        ..Default::default()
    };
    Ok(ratex_svg::render_to_svg(&list, &options))
}

fn mermaid_svg(source: &str) -> Result<String, String> {
    mermaid_rs_renderer::render(source).map_err(|e| e.to_string())
}

/// The size an SVG says it is: its `width` and `height`, else its `viewBox`.
fn size_of(svg: &str) -> Option<(f32, f32)> {
    let head = &svg[svg.find("<svg")?..];
    let head = &head[..head.find('>')?];
    let attr = |name: &str| -> Option<&str> {
        let at = head.find(&format!(" {name}=\""))? + name.len() + 3;
        Some(&head[at..at + head[at..].find('"')?])
    };
    let number = |v: &str| -> Option<f32> {
        let v = v.trim().trim_end_matches("px").trim_end_matches("pt");
        v.parse::<f32>().ok().filter(|n| *n > 0.0)
    };
    if let (Some(w), Some(h)) = (attr("width").and_then(number), attr("height").and_then(number)) {
        return Some((w, h));
    }
    let view: Vec<f32> = attr("viewBox")?
        .split([' ', ','])
        .filter(|s| !s.is_empty())
        .filter_map(|n| n.parse().ok())
        .collect();
    match view[..] {
        [_, _, w, h] if w > 0.0 && h > 0.0 => Some((w, h)),
        _ => None,
    }
}

/// `markdown` with its display math made into blocks of `math` and its math inside a line written
/// in Unicode; itself when it has none.
/// The mark a GitHub alert's first line is turned into: a paragraph of its own, which the reply's
/// quote finds and draws as the alert's head.
pub const ALERT: char = '\u{2060}';

/// GitHub's alerts, `> [!NOTE]` and the rest, as a quote whose first paragraph is the mark and the
/// kind; and `<details>`, which is not drawn, as its summary in bold over what it holds. Not in
/// code.
pub fn alerts(markdown: &str) -> Cow<'_, str> {
    if !markdown.contains("[!") && !markdown.contains("<details") {
        return Cow::Borrowed(markdown);
    }
    let mut out = String::with_capacity(markdown.len() + 16);
    let mut fence = false;
    for line in markdown.split_inclusive('\n') {
        let bare = line.trim();
        if bare.starts_with("```") || bare.starts_with("~~~") {
            fence = !fence;
        }
        if !fence {
            if let Some(kind) = bare
                .strip_prefix('>')
                .map(str::trim)
                .and_then(|r| r.strip_prefix("[!"))
                .and_then(|r| r.strip_suffix(']'))
                .filter(|k| {
                    ["NOTE", "TIP", "IMPORTANT", "WARNING", "CAUTION"].contains(&k.to_ascii_uppercase().as_str())
                })
            {
                out.push_str(&format!("> {ALERT}{}\n>\n", kind.to_ascii_uppercase()));
                continue;
            }
            if bare == "<details>" || bare == "</details>" || bare.starts_with("<details ") {
                continue;
            }
            if let Some(summary) = bare
                .strip_prefix("<summary>")
                .and_then(|r| r.strip_suffix("</summary>"))
            {
                out.push_str(&format!("**▸ {}**\n\n", summary.trim()));
                continue;
            }
        }
        out.push_str(line);
    }
    Cow::Owned(out)
}

pub fn prepare(markdown: &str) -> Cow<'_, str> {
    if !markdown.contains('$') && !markdown.contains("\\[") && !markdown.contains("\\(") {
        return Cow::Borrowed(markdown);
    }
    let mut out = String::with_capacity(markdown.len() + 16);
    // The fence of the code block the line is in: its character, and how many.
    let mut fence: Option<(char, usize)> = None;
    // The display math the line is in: how it opened, how it closes, and what it has so far.
    let mut display: Option<(&str, &str, String)> = None;
    let fence_of = |bare: &str| -> Option<(char, usize)> {
        let c = bare.chars().next().filter(|c| *c == '`' || *c == '~')?;
        let n = bare.chars().take_while(|x| *x == c).count();
        (n >= 3).then_some((c, n))
    };
    for line in markdown.split_inclusive('\n') {
        let bare = line.trim();
        if let Some((_, close, body)) = &mut display {
            match bare.strip_suffix(*close) {
                Some(last) => {
                    body.push_str(last);
                    out.push_str(&format!("```math\n{}\n```\n", body.trim()));
                    display = None;
                }
                None => body.push_str(line),
            }
            continue;
        }
        if let Some((c, n)) = fence {
            if fence_of(bare).is_some_and(|(d, m)| d == c && m >= n && bare.trim_end_matches(c).is_empty()) {
                fence = None;
            }
            out.push_str(line);
            continue;
        }
        if let Some(opened) = fence_of(bare) {
            fence = Some(opened);
            out.push_str(line);
            continue;
        }
        // Display math: alone on its lines, or whole on one.
        let opened = [("$$", "$$"), ("\\[", "\\]")]
            .into_iter()
            .find(|(open, _)| bare.starts_with(open));
        if let Some((open, close)) = opened {
            let rest = &bare[open.len()..];
            if let Some(whole) = rest.strip_suffix(close).filter(|_| !rest.is_empty()) {
                out.push_str(&format!("```math\n{}\n```\n", whole.trim()));
                continue;
            }
            if !rest.contains(close) {
                display = Some((open, close, format!("{rest}\n")));
                continue;
            }
        }
        out.push_str(&inline(line));
    }
    // Still open at the end, as it is while it streams: as it was written.
    if let Some((open, _, body)) = display {
        out.push_str(open);
        out.push_str(&body);
    }
    Cow::Owned(out)
}

/// A line with its math, `$…$` or `\(…\)`, in Unicode; its code spans as they are. A `$` is math's
/// only when the next is not a space and the closing one follows no space and comes before no digit:
/// "$5 and $10" is money.
fn inline(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '`' {
            let ticks = chars[i..].iter().take_while(|c| **c == '`').count();
            let close = (i + ticks..chars.len()).find(|&j| {
                chars[j..].iter().take_while(|c| **c == '`').count() == ticks && (j == 0 || chars[j - 1] != '`')
            });
            let end = close.map_or(chars.len(), |j| j + ticks);
            out.extend(&chars[i..end]);
            i = end;
            continue;
        }
        if c == '\\' && chars.get(i + 1) == Some(&'(') {
            let close = (i + 2..chars.len().saturating_sub(1)).find(|&j| chars[j] == '\\' && chars[j + 1] == ')');
            if let Some(j) = close {
                out.push_str(&unicode(&chars[i + 2..j].iter().collect::<String>()));
                i = j + 2;
                continue;
            }
        }
        if c == '$'
            && chars.get(i + 1).is_some_and(|n| !n.is_whitespace() && *n != '$')
            && (i == 0 || chars[i - 1] != '\\')
        {
            let close = (i + 2..chars.len()).find(|&j| {
                chars[j] == '$'
                    && !chars[j - 1].is_whitespace()
                    && chars[j - 1] != '\\'
                    && !chars.get(j + 1).is_some_and(|n| n.is_ascii_digit())
            });
            if let Some(j) = close {
                out.push_str(&unicode(&chars[i + 1..j].iter().collect::<String>()));
                i = j + 1;
                continue;
            }
        }
        out.push(c);
        i += 1;
    }
    out
}

/// TeX written as far as Unicode goes: letters and symbols by their names, sub- and superscripts
/// where they have characters, a fraction as `a/b`, a root as `√`. What it does not know is kept.
pub fn unicode(tex: &str) -> String {
    let mut out = String::new();
    let chars: Vec<char> = tex.trim().chars().collect();
    let mut i = 0;
    // The group starting at `i`: `{…}` or one character, or one command; and where it ends.
    fn group(chars: &[char], i: usize) -> (String, usize) {
        match chars.get(i) {
            Some('{') => {
                let mut depth = 0;
                for j in i..chars.len() {
                    match chars[j] {
                        '{' => depth += 1,
                        '}' => {
                            depth -= 1;
                            if depth == 0 {
                                return (chars[i + 1..j].iter().collect(), j + 1);
                            }
                        }
                        _ => {}
                    }
                }
                (chars[i + 1..].iter().collect(), chars.len())
            }
            Some('\\') => {
                let end = (i + 1..chars.len())
                    .find(|&j| !chars[j].is_ascii_alphabetic())
                    .map_or(chars.len(), |j| if j == i + 1 { j + 1 } else { j });
                (chars[i..end.min(chars.len())].iter().collect(), end.min(chars.len()))
            }
            Some(c) => (c.to_string(), i + 1),
            None => (String::new(), i),
        }
    }
    while i < chars.len() {
        let c = chars[i];
        match c {
            '^' | '_' => {
                let (g, next) = group(&chars, i + 1);
                let inner = unicode(&g);
                let small: Option<String> = inner
                    .chars()
                    .map(|ch| if c == '^' { superscript(ch) } else { subscript(ch) })
                    .collect();
                match small {
                    Some(s) => out.push_str(&s),
                    None if inner.chars().count() == 1 => {
                        out.push(c);
                        out.push_str(&inner);
                    }
                    None => out.push_str(&format!("{c}({inner})")),
                }
                i = next;
            }
            '{' | '}' => i += 1,
            '\\' => {
                let (name, next) = group(&chars, i);
                let name = &name[1..];
                i = next;
                match name {
                    "frac" | "dfrac" | "tfrac" => {
                        let (a, n1) = group(&chars, i);
                        let (b, n2) = group(&chars, n1);
                        let (a, b) = (unicode(&a), unicode(&b));
                        let wrap = |s: String| {
                            if s.chars().count() > 1 && s.contains([' ', '+', '-', '−']) {
                                format!("({s})")
                            } else {
                                s
                            }
                        };
                        out.push_str(&format!("{}/{}", wrap(a), wrap(b)));
                        i = n2;
                    }
                    "sqrt" => {
                        let (a, n) = group(&chars, i);
                        let a = unicode(&a);
                        if a.chars().count() > 1 {
                            out.push_str(&format!("√({a})"));
                        } else {
                            out.push_str(&format!("√{a}"));
                        }
                        i = n;
                    }
                    "text" | "mathrm" | "mathbf" | "mathit" | "operatorname" | "mathsf" | "mathtt" | "textbf"
                    | "textit" | "boldsymbol" => {
                        let (a, n) = group(&chars, i);
                        out.push_str(&if name.starts_with("text") { a } else { unicode(&a) });
                        i = n;
                    }
                    "mathbb" => {
                        let (a, n) = group(&chars, i);
                        out.extend(a.chars().map(double_struck));
                        i = n;
                    }
                    "left" | "right" | "big" | "Big" | "bigg" | "Bigg" | "displaystyle" | "limits" => {}
                    "," | ";" | ":" | "!" | " " | "quad" | "qquad" => out.push(' '),
                    "\\" => out.push(' '),
                    other => match symbol(other) {
                        Some(s) => out.push_str(s),
                        None if other.len() == 1 => out.push_str(other),
                        None => {
                            out.push('\\');
                            out.push_str(other);
                        }
                    },
                }
            }
            '-' => {
                out.push('−');
                i += 1;
            }
            _ => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

fn superscript(c: char) -> Option<char> {
    Some(match c {
        '0' => '⁰',
        '1' => '¹',
        '2' => '²',
        '3' => '³',
        '4' => '⁴',
        '5' => '⁵',
        '6' => '⁶',
        '7' => '⁷',
        '8' => '⁸',
        '9' => '⁹',
        '+' => '⁺',
        '-' | '−' => '⁻',
        '=' => '⁼',
        '(' => '⁽',
        ')' => '⁾',
        'n' => 'ⁿ',
        'i' => 'ⁱ',
        'a' => 'ᵃ',
        'b' => 'ᵇ',
        'c' => 'ᶜ',
        'd' => 'ᵈ',
        'e' => 'ᵉ',
        'k' => 'ᵏ',
        'm' => 'ᵐ',
        'o' => 'ᵒ',
        'p' => 'ᵖ',
        't' => 'ᵗ',
        'x' => 'ˣ',
        'T' => 'ᵀ',
        '′' => '′',
        '*' => '*',
        _ => return None,
    })
}

fn subscript(c: char) -> Option<char> {
    Some(match c {
        '0' => '₀',
        '1' => '₁',
        '2' => '₂',
        '3' => '₃',
        '4' => '₄',
        '5' => '₅',
        '6' => '₆',
        '7' => '₇',
        '8' => '₈',
        '9' => '₉',
        '+' => '₊',
        '-' | '−' => '₋',
        '=' => '₌',
        '(' => '₍',
        ')' => '₎',
        'a' => 'ₐ',
        'e' => 'ₑ',
        'i' => 'ᵢ',
        'j' => 'ⱼ',
        'k' => 'ₖ',
        'm' => 'ₘ',
        'n' => 'ₙ',
        'o' => 'ₒ',
        'p' => 'ₚ',
        'r' => 'ᵣ',
        's' => 'ₛ',
        't' => 'ₜ',
        'u' => 'ᵤ',
        'x' => 'ₓ',
        _ => return None,
    })
}

fn double_struck(c: char) -> char {
    match c {
        'R' => 'ℝ',
        'N' => 'ℕ',
        'Z' => 'ℤ',
        'Q' => 'ℚ',
        'C' => 'ℂ',
        'P' => 'ℙ',
        'E' => '𝔼',
        other => other,
    }
}

fn symbol(name: &str) -> Option<&'static str> {
    Some(match name {
        "alpha" => "α",
        "beta" => "β",
        "gamma" => "γ",
        "delta" => "δ",
        "epsilon" | "varepsilon" => "ε",
        "zeta" => "ζ",
        "eta" => "η",
        "theta" | "vartheta" => "θ",
        "iota" => "ι",
        "kappa" => "κ",
        "lambda" => "λ",
        "mu" => "μ",
        "nu" => "ν",
        "xi" => "ξ",
        "pi" => "π",
        "rho" => "ρ",
        "sigma" => "σ",
        "tau" => "τ",
        "upsilon" => "υ",
        "phi" | "varphi" => "φ",
        "chi" => "χ",
        "psi" => "ψ",
        "omega" => "ω",
        "Gamma" => "Γ",
        "Delta" => "Δ",
        "Theta" => "Θ",
        "Lambda" => "Λ",
        "Xi" => "Ξ",
        "Pi" => "Π",
        "Sigma" => "Σ",
        "Phi" => "Φ",
        "Psi" => "Ψ",
        "Omega" => "Ω",
        "times" => "×",
        "cdot" => "·",
        "div" => "÷",
        "pm" => "±",
        "mp" => "∓",
        "le" | "leq" => "≤",
        "ge" | "geq" => "≥",
        "ne" | "neq" => "≠",
        "approx" => "≈",
        "equiv" => "≡",
        "sim" => "∼",
        "propto" => "∝",
        "infty" => "∞",
        "sum" => "∑",
        "prod" => "∏",
        "int" => "∫",
        "oint" => "∮",
        "partial" => "∂",
        "nabla" => "∇",
        "forall" => "∀",
        "exists" => "∃",
        "in" => "∈",
        "notin" => "∉",
        "subset" => "⊂",
        "subseteq" => "⊆",
        "supset" => "⊃",
        "supseteq" => "⊇",
        "cup" => "∪",
        "cap" => "∩",
        "emptyset" | "varnothing" => "∅",
        "to" | "rightarrow" => "→",
        "leftarrow" | "gets" => "←",
        "Rightarrow" | "implies" => "⇒",
        "Leftarrow" => "⇐",
        "leftrightarrow" => "↔",
        "Leftrightarrow" | "iff" => "⇔",
        "mapsto" => "↦",
        "land" | "wedge" => "∧",
        "lor" | "vee" => "∨",
        "neg" | "lnot" => "¬",
        "cdots" | "ldots" | "dots" => "…",
        "circ" => "∘",
        "deg" => "°",
        "prime" => "′",
        "log" => "log",
        "ln" => "ln",
        "exp" => "exp",
        "sin" => "sin",
        "cos" => "cos",
        "tan" => "tan",
        "lim" => "lim",
        "max" => "max",
        "min" => "min",
        "langle" => "⟨",
        "rangle" => "⟩",
        "lfloor" => "⌊",
        "rfloor" => "⌋",
        "lceil" => "⌈",
        "rceil" => "⌉",
        "{" => "{",
        "}" => "}",
        "%" => "%",
        "$" => "$",
        "_" => "_",
        "&" => "&",
        "#" => "#",
        "|" => "‖",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_alert_gets_a_head_of_its_own_and_details_show_their_summary() {
        let md =
            "> [!warning]\n> 消えます\n\n<details>\n<summary>ログ</summary>\n\n中身\n</details>\n```\n> [!NOTE]\n```\n";
        let out = alerts(md);
        assert!(out.starts_with(&format!("> {ALERT}WARNING\n>\n> 消えます\n")), "{out}");
        assert!(out.contains("**▸ ログ**\n\n\n中身\n```"), "{out}");
        assert!(
            out.contains("```\n> [!NOTE]\n```"),
            "in code it is left as it is: {out}"
        );
        assert!(matches!(alerts("ふつう"), Cow::Borrowed(_)));
    }

    #[test]
    fn display_math_becomes_a_block_and_inline_math_unicode_but_code_and_money_stay() {
        let md = "式は\n\n$$\n\\frac{a}{b}\n$$\n\nまた $x^2 + y_i$ と \\(\\alpha \\le \\beta\\)。\n\n`$HOME` は$5と$10の間\n\n```sh\necho $$ $x$\n```\n\\[E = mc^2\\]\n";
        let out = prepare(md);
        assert_eq!(
            out,
            "式は\n\n```math\n\\frac{a}{b}\n```\n\nまた x² + yᵢ と α ≤ β。\n\n`$HOME` は$5と$10の間\n\n```sh\necho $$ $x$\n```\n```math\nE = mc^2\n```\n"
        );
        assert!(matches!(prepare("ただの文"), Cow::Borrowed(_)));
    }

    #[test]
    fn tex_is_written_in_unicode_as_far_as_it_goes() {
        assert_eq!(unicode(r"\frac{1}{2} \cdot \sqrt{x+1}"), "1/2 · √(x+1)");
        assert_eq!(unicode(r"\sum_{i=1}^{n} a_i"), "∑ᵢ₌₁ⁿ aᵢ");
        assert_eq!(unicode(r"x \in \mathbb{R}"), "x ∈ ℝ");
        assert_eq!(unicode(r"O(n \log n)"), "O(n log n)");
        assert_eq!(unicode(r"\text{rate} = k_{\max}"), "rate = kₘₐₓ");
        assert_eq!(unicode(r"\unknown{x}"), r"\unknownx");
    }

    #[test]
    fn math_and_a_diagram_are_drawn_with_their_size_and_a_mistake_says_so() {
        let math = figure(true, r"\int_0^\infty e^{-x^2}\,dx = \frac{\sqrt{\pi}}{2}").expect("math is laid out");
        assert!(math.width > 50.0 && math.height > 10.0);
        let diagram = figure(false, "flowchart LR\n  A[入力] --> B{判定}\n  B -->|はい| C[送る]").expect("a diagram");
        assert!(diagram.width > 100.0 && diagram.height > 20.0);
        assert!(figure(true, r"\frac{a").is_err());
        assert!(figure(false, "not a diagram at all").is_err());
    }

    #[test]
    fn the_size_of_an_svg_is_read_from_its_attributes_or_its_view() {
        assert_eq!(size_of(r#"<svg width="12.5" height="4pt">"#), Some((12.5, 4.0)));
        assert_eq!(
            size_of(r#"<?xml?><svg viewBox="0 0 300 120" xmlns="x">"#),
            Some((300.0, 120.0))
        );
        assert_eq!(size_of("<svg>"), None);
    }
}
