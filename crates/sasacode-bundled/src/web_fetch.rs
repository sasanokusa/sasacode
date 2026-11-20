//! web_fetch (`packages/bundled/src/web-fetch.ts`): read a URL as text. HTML is cut down to its
//! content (`readable`, on lol-html as Bun's HTMLRewriter was) and turned into plain text whose
//! links are numbered absolute URLs (`html_to_text`).

use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::LazyLock;
use std::time::Duration;

use futures::StreamExt;
use futures::future::BoxFuture;
use lol_html::html_content::{ContentType, Element, TextChunk};
use lol_html::{ElementContentHandlers, RewriteStrSettings, Selector, rewrite_str};
use regex::{Captures, Regex};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use unicode_normalization::UnicodeNormalization;

use sasacode_plugin_api::{PluginApi, ToolDef, ToolKind, ToolResult, error_result};

use crate::net_guard::{Check, Limit, Route, SYSTEM, get_following};
use crate::net_util::{collapse_space, js_trim, utf16_len, utf16_slice};

const DEFAULT_MAX: usize = 50_000;
/// Read at most this much of a response: a huge page must not fill memory.
const MAX_BYTES: usize = 10_000_000;
/// Redirects followed before giving up.
const MAX_REDIRECTS: usize = 10;

pub const USER_AGENT: &str = "sasacode (+https://github.com/sasanokusa/sasacode)";

fn re(pattern: &str) -> Regex {
    // INFALLIBLE: every pattern here is a fixed literal, checked by the tests.
    Regex::new(pattern).expect("fixed pattern")
}

macro_rules! regex {
    ($name:ident, $pattern:expr) => {
        static $name: LazyLock<Regex> = LazyLock::new(|| re($pattern));
    };
}

/// What [`read_capped`] read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capped {
    pub text: String,
    /// The document went on past the limit.
    pub cut: bool,
}

/// Read at most `limit` bytes of a response, saying whether the document went on past them.
pub async fn read_capped(res: reqwest::Response, limit: usize) -> anyhow::Result<Capped> {
    let content_type = res
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .map(|v| String::from_utf8_lossy(v.as_bytes()).into_owned());
    let mut stream = res.bytes_stream();
    let mut bytes: Vec<u8> = Vec::new();
    while bytes.len() < limit {
        match stream.next().await {
            None => break,
            Some(chunk) => bytes.extend_from_slice(&chunk?),
        }
    }
    // Two ways the limit can be reached, and only one of them is a cut: a chunk that overran it took
    // the rest of the document with it, while a document that ends exactly at the limit needs one
    // more read to tell "it ended here" from "there is more" (the note in the tool would otherwise
    // claim a 10 MB page was cut when it was read whole).
    // `reader.read().then((r) => r.done, () => false)`: more data, or an error, is a cut.
    let cut = bytes.len() > limit || (bytes.len() == limit && more(&mut stream).await);
    drop(stream);
    bytes.truncate(limit);
    Ok(Capped {
        text: decode(content_type.as_deref(), &bytes),
        cut,
    })
}

/// Whether the stream has more to give: a chunk (even an empty one) or an error, not its end.
async fn more<S, B, E>(stream: &mut S) -> bool
where
    S: futures::Stream<Item = Result<B, E>> + Unpin,
{
    stream.next().await.is_some()
}

regex!(CHARSET, r#"(?i)charset\s*=\s*["']?([A-Za-z0-9_.:-]+)"#);
regex!(META_CHARSET, r#"(?i)<meta[^>]+charset\s*=\s*["']?([A-Za-z0-9_.:-]+)"#);

/// Decode with the charset the response names (Content-Type, else a <meta> near the start), UTF-8
/// when it names none or one TextDecoder does not know: a Shift_JIS or EUC-JP page read as UTF-8 was
/// all U+FFFD (#120). A BOM is dropped only when it is the encoding's own, as TextDecoder does.
fn decode(content_type: Option<&str>, bytes: &[u8]) -> String {
    let head: String = bytes.iter().take(4096).map(|b| char::from(*b)).collect();
    let charset = CHARSET
        .captures(content_type.unwrap_or(""))
        .or_else(|| META_CHARSET.captures(&head))
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_string());
    let encoding = charset
        .and_then(|c| encoding_rs::Encoding::for_label_no_replacement(c.as_bytes()))
        .unwrap_or(encoding_rs::UTF_8);
    encoding.decode_with_bom_removal(bytes).0.into_owned()
}

/// Named references htmlToText decodes; anything else is left as written.
static ENTITIES: LazyLock<HashMap<String, String>> = LazyLock::new(|| {
    let mut m: HashMap<String, String> = [
        ("amp", "&"),
        ("lt", "<"),
        ("gt", ">"),
        ("quot", "\""),
        ("apos", "'"),
        ("nbsp", " "),
        ("copy", "©"),
        ("reg", "®"),
        ("trade", "™"),
        ("hellip", "…"),
        ("middot", "·"),
        ("bull", "•"),
        ("mdash", "—"),
        ("ndash", "–"),
        ("lsquo", "‘"),
        ("rsquo", "’"),
        ("ldquo", "“"),
        ("rdquo", "”"),
        ("laquo", "«"),
        ("raquo", "»"),
        ("lsaquo", "‹"),
        ("rsaquo", "›"),
        ("times", "×"),
        ("deg", "°"),
        ("euro", "€"),
        ("pound", "£"),
        ("yen", "¥"),
        ("szlig", "ß"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();
    // Accented letters: &auml; &Eacute; &ccedil; …
    for (mark, combining) in [
        ("uml", '\u{0308}'),
        ("acute", '\u{0301}'),
        ("grave", '\u{0300}'),
        ("circ", '\u{0302}'),
        ("tilde", '\u{0303}'),
        ("ring", '\u{030a}'),
        ("cedil", '\u{0327}'),
    ] {
        for letter in "aeiouyncAEIOUYNC".chars() {
            let composed: String = [letter, combining].iter().collect::<String>().nfc().collect();
            if composed.chars().count() == 1 {
                m.insert(format!("{letter}{mark}"), composed);
            }
        }
    }
    m
});

/// "#65" or "#x41" (or "#X41") as its character; one past the last code point (or not a number) is
/// left as written. Like `Number.parseInt`, only the leading digits count.
fn code_point(r: &str) -> Option<String> {
    let hex = matches!(r.as_bytes().get(1), Some(b'x' | b'X'));
    let digits: String = r[if hex { 2 } else { 1 }..]
        .chars()
        .take_while(|c| if hex { c.is_ascii_hexdigit() } else { c.is_ascii_digit() })
        .collect();
    if digits.is_empty() {
        return None;
    }
    let n = u32::from_str_radix(&digits, if hex { 16 } else { 10 }).ok()?;
    if n > 0x10ffff {
        return None;
    }
    // A surrogate code point: JS made a lone surrogate of it, which Rust strings cannot hold.
    Some(char::from_u32(n).unwrap_or('\u{fffd}').to_string())
}

regex!(ENTITY, r"(?i)&(#x?[0-9a-f]+|[A-Za-z0-9_]+);");

fn entities(s: &str) -> String {
    ENTITY
        .replace_all(s, |c: &Captures| {
            let e = &c[1];
            let found = if e.starts_with('#') {
                code_point(e)
            } else {
                ENTITIES.get(e).cloned()
            };
            found.unwrap_or_else(|| c[0].to_string())
        })
        .into_owned()
}

/// Page furniture: never the content an agent reads a page for (menus, language lists, inline code, banners).
const FURNITURE: &str = concat!(
    "script, style, noscript, template, svg, iframe, object, canvas, button, select, dialog, nav, aside, ",
    "[role=navigation], [role=banner], [role=contentinfo], [role=complementary], [role=search], [role=dialog], ",
    "body > header, body > footer, #header, #footer, .site-header, .site-footer, ",
    // MediaWiki (Wikipedia): edit links, tables of contents, navigation boxes, maintenance notices, tools
    ".mw-editsection, .mw-jump-link, #toc, .toc, .vector-toc, .navbox, .vertical-navbox, .ambox, .mbox-small, ",
    ".metadata, .noprint, .catlinks, .printfooter, #siteSub, .mw-indicators, .vector-page-toolbar, .vector-menu, .mw-portlet",
);

/// Hidden elements: furniture outside the content region; inside it, often content (tabs, accordions), so kept there.
const HIDDEN: &str = "[hidden], [aria-hidden=true]";

/// Where a page keeps its content, most specific first: the first of these with real text is what is read.
const CONTENT: [&str; 9] = [
    "#mw-content-text",
    "[role=main]",
    "main",
    "article",
    "#content",
    "#main",
    ".entry-content",
    ".post-content",
    ".article-body",
];

regex!(TAG_OR_COMMENT, r"(?s)<!--.*?-->|<[^>]+>");

/// Text characters in some HTML, roughly: what tells a content region from an empty frame.
fn text_length(html: &str) -> usize {
    utf16_len(&collapse_space(&TAG_OR_COMMENT.replace_all(html, "")))
}

/// The share of the page's text a content region must hold: less is a card or a comment, not the content.
const CONTENT_SHARE: f64 = 0.2;

/// Site navigation links kept, at most: the way on to the site's other pages (a docs sidebar), without its bulk.
const SITE_LINKS: usize = 25;

type Handlers<'h> = (Cow<'static, Selector>, ElementContentHandlers<'h>);

fn on_element<'h>(
    selector: &str,
    f: impl FnMut(&mut Element<'_, '_>) -> lol_html::HandlerResult + 'h,
) -> Result<Handlers<'h>, lol_html::errors::SelectorError> {
    Ok((
        Cow::Owned(selector.parse::<Selector>()?),
        ElementContentHandlers::default().element(f),
    ))
}

fn on_text<'h>(
    selector: &str,
    f: impl FnMut(&mut TextChunk<'_>) -> lol_html::HandlerResult + 'h,
) -> Result<Handlers<'h>, lol_html::errors::SelectorError> {
    Ok((
        Cow::Owned(selector.parse::<Selector>()?),
        ElementContentHandlers::default().text(f),
    ))
}

fn rewrite(html: &str, handlers: Vec<Handlers<'_>>) -> Option<String> {
    let mut settings = RewriteStrSettings::new();
    for h in handlers {
        settings = settings.append_element_content_handler(h);
    }
    rewrite_str(html, settings).ok()
}

regex!(HEAD, r"(?is)<head.*?</head>");
regex!(MARK, r"<!--sx(\d+)\.(\d+)-->");
regex!(BASE_TAG, r"(?i)<base\s[^>]*>");
regex!(TITLE, r"(?is)<title[^>]*>(.*?)</title>");
regex!(H1, r"(?i)<h1[\s>]");

/// The marks [`readable`] puts around each content region, then the cleaned page.
fn marked(html: &str) -> Option<(String, Vec<(String, String)>)> {
    let n = RefCell::new(0usize);
    let nav: RefCell<Vec<(String, String)>> = RefCell::new(Vec::new());
    let mut handlers = vec![
        on_element(FURNITURE, |e| {
            e.remove();
            Ok(())
        })
        .ok()?,
        // Removed with their navigation, and gathered first.
        on_element("nav a[href], [role=navigation] a[href]", |e| {
            nav.borrow_mut()
                .push((e.get_attribute("href").unwrap_or_default(), String::new()));
            Ok(())
        })
        .ok()?,
        on_text("nav a[href], [role=navigation] a[href]", |t| {
            if let Some(link) = nav.borrow_mut().last_mut() {
                link.1.push_str(t.as_str());
            }
            Ok(())
        })
        .ok()?,
        // An attribute holding < or > (MediaWiki's data-mw) would end a tag early for htmlToText's patterns.
        on_element("*", |e| {
            let names: Vec<String> = e
                .attributes()
                .iter()
                .filter(|a| a.value().contains(['<', '>']))
                .map(|a| a.name())
                .collect();
            for name in names {
                e.remove_attribute(&name);
            }
            Ok(())
        })
        .ok()?,
    ];
    for (i, selector) in CONTENT.iter().enumerate() {
        let n = &n;
        handlers.push(
            on_element(selector, move |e| {
                let k = {
                    let mut c = n.borrow_mut();
                    *c += 1;
                    *c - 1
                };
                e.prepend(&format!("<!--sx{i}.{k}-->"), ContentType::Html);
                e.append(&format!("<!--/sx{i}.{k}-->"), ContentType::Html);
                Ok(())
            })
            .ok()?,
        );
    }
    let cleaned = rewrite(html, handlers)?;
    Some((cleaned, nav.into_inner()))
}

/// The part of a page worth reading: furniture removed, and only the content region when the page
/// marks one (main, article, MediaWiki's content) that holds most of its text. What html_to_text then
/// reads. With `site_links`, the navigation's links follow the content as one short line.
pub fn readable(html: &str, site_links: bool) -> String {
    // Not HTML lol-html can read: leave it to html_to_text as it is.
    let Some((cleaned, nav)) = marked(html) else {
        return html.to_string();
    };
    let site = || {
        if !site_links {
            return String::new();
        }
        let mut seen = std::collections::HashSet::new();
        let kept: Vec<(String, String)> = nav
            .iter()
            .map(|(href, label)| (js_trim(href).to_string(), js_trim(&collapse_space(label)).to_string()))
            .filter(|(href, label)| {
                !label.is_empty() && !href.is_empty() && !href.starts_with('#') && seen.insert(href.clone())
            })
            .take(SITE_LINKS)
            .collect();
        if kept.is_empty() {
            return String::new();
        }
        // Both as the page wrote them (HTML, entities included): only what would end the attribute is escaped.
        let links: Vec<String> = kept
            .iter()
            .map(|(href, label)| format!("<a href=\"{}\">{label}</a>", href.replace('"', "&quot;")))
            .collect();
        format!("<p>Site navigation: {}</p>", links.join(" · "))
    };
    let whole = text_length(&HEAD.replace(&cleaned, ""));
    // The marks, by the content selector they stand for.
    let marks: Vec<(usize, usize, String)> = MARK
        .captures_iter(&cleaned)
        .filter_map(|c| {
            let at = c.get(0)?.start();
            Some((c[1].parse::<usize>().ok()?, at, c[2].to_string()))
        })
        .collect();
    for i in 0..CONTENT.len() {
        // Each match on its own (a page can have many <article>s: cards, comments): the one with the most text.
        let mut best = "";
        let mut best_length = 0;
        for (_, at, k) in marks.iter().filter(|(j, _, _)| *j == i) {
            let close = format!("<!--/sx{i}.{k}-->");
            let end = cleaned[*at..].find(&close).map_or(cleaned.len(), |e| at + e);
            let region = &cleaned[*at..end];
            let length = text_length(region);
            if length > best_length {
                best = region;
                best_length = length;
            }
        }
        if best_length < 200 || (best_length as f64) < whole as f64 * CONTENT_SHARE {
            continue;
        }
        // What the region leaves behind and the reading needs: the base for relative links, the page's title.
        let base = BASE_TAG.find(html).map_or("", |m| m.as_str());
        let title = TITLE
            .captures(html)
            .map(|c| js_trim(&c[1]).to_string())
            .filter(|t| !t.is_empty());
        let heading = match title {
            Some(t) if !H1.is_match(best) => format!("<h1>{t}</h1>"),
            _ => String::new(),
        };
        return format!("{base}{heading}{best}{}", site());
    }
    // No content region: the whole page, its hidden parts left out too.
    let shown = on_element(HIDDEN, |e| {
        e.remove();
        Ok(())
    })
    .ok()
    .and_then(|h| rewrite(&cleaned, vec![h]))
    .unwrap_or(cleaned);
    let lower = shown.to_ascii_lowercase();
    match lower.find("</body>") {
        Some(at) => format!("{}{}{}", &shown[..at], site(), &shown[at..]),
        None => format!("{shown}{}", site()),
    }
}

regex!(AMP, r"(?i)&amp;");
regex!(NOWHERE, r"(?i)^(javascript|mailto|tel|data):");

/// A link's target as an absolute URL; None for one that goes nowhere else (same page, script, mail).
fn link_target(href: &str, base: Option<&str>) -> Option<String> {
    let raw = AMP.replace_all(js_trim(href), "&");
    if raw.is_empty() || raw.starts_with('#') || NOWHERE.is_match(&raw) {
        return None;
    }
    let url = match base {
        Some(b) => url::Url::parse(b).ok()?.join(&raw).ok()?,
        None => url::Url::parse(&raw).ok()?,
    };
    if url.scheme() != "http" && url.scheme() != "https" {
        return None;
    }
    let href = url.to_string();
    if let Some(b) = base
        && href.split('#').next() == b.split('#').next()
    {
        return None;
    }
    Some(href)
}

/// What [`html_to_text`] made of a page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HtmlText {
    pub text: String,
    /// Link targets; a reference "[label][n]" in the text points at `links[n - 1]`.
    pub links: Vec<String>,
}

regex!(DECLARED_BASE, r#"(?i)<base\s[^>]*href\s*=\s*["']([^"']+)["']"#);
regex!(BLOCK_OPEN, r"(?i)<(script|style|noscript|svg|head)");
regex!(COMMENT, r"(?s)<!--.*?-->");
regex!(LINK, r"(?is)<a\s([^>]*)>(.*?)</a>");
regex!(HREF, r#"(?i)(?-u:\b)href\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s>]+))"#);
regex!(TAG, r"<[^>]+>");
regex!(PRE, r"(?is)<pre[^>]*>(.*?)</pre>");
regex!(BR, r"(?i)<br[^>]*>");
regex!(CELL_END, r"(?i)</t[dh]>");
regex!(
    BREAK,
    r"(?i)<(br|/p|/div|/li|/h[1-6]|/tr|/dt|/dd|/table|/section|/article)[^>]*>"
);
regex!(LI, r"(?i)<li[^>]*>");
regex!(SPACES, r"[ \t]+");
regex!(ROW_END, r"(?mR) \| *$");
regex!(EMPTY_BULLETS, r"(?mR)^ ?(?:- ?)+$");
regex!(BLANK_LINES, r"\n\s*\n\s*\n+");
regex!(PRE_MARK, r"\x00(\d+)\x00");

/// `html.replace(/<(script|style|noscript|svg|head)[\s\S]*?<\/\1>/gi, "")`: each such element up to
/// its own closing tag (the backreference, which `regex` does not have).
fn without_blocks(html: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let mut out = String::with_capacity(html.len());
    let (mut copied, mut from) = (0, 0);
    while let Some(c) = BLOCK_OPEN.captures_at(html, from) {
        let Some(open) = c.get(0) else { break };
        let close = format!("</{}>", c[1].to_ascii_lowercase());
        match lower[open.end()..].find(&close) {
            Some(rel) => {
                out.push_str(&html[copied..open.start()]);
                copied = open.end() + rel + close.len();
                from = copied;
            }
            // `<` is one byte: the next position is a character boundary.
            None => from = open.start() + 1,
        }
    }
    out.push_str(&html[copied..]);
    out
}

/// HTML as plain text. Links become markdown references ("[Docs][3]") into `links`, absolute URLs
/// resolved against `base` (or the page's own <base href>), so the agent can follow a link instead
/// of putting a URL together from a relative path or from memory.
pub fn html_to_text(html: &str, base: Option<&str>) -> HtmlText {
    let mut base = base.map(str::to_string);
    if let Some(declared) = DECLARED_BASE.captures(html).map(|c| c[1].to_string()) {
        base = link_target(&declared, base.as_deref()).or(base);
    }
    let mut links: Vec<String> = Vec::new();
    let mut numbers: HashMap<String, usize> = HashMap::new();
    let mut pre: Vec<String> = Vec::new();
    let text = without_blocks(html);
    let text = COMMENT.replace_all(&text, "");
    let text = LINK.replace_all(&text, |c: &Captures| {
        let (attrs, inner) = (&c[1], &c[2]);
        let target = HREF.captures(attrs).and_then(|h| {
            let href = h.get(1).or(h.get(2)).or(h.get(3)).map_or("", |m| m.as_str());
            link_target(href, base.as_deref())
        });
        let label = js_trim(&collapse_space(&TAG.replace_all(inner, ""))).to_string();
        match target {
            Some(target) if !label.is_empty() => {
                let n = *numbers.entry(target.clone()).or_insert_with(|| {
                    links.push(target);
                    links.len()
                });
                format!("[{label}][{n}]")
            }
            _ => inner.to_string(),
        }
    });
    // Set aside as it is: its spaces are indentation, which code and YAML need (#120).
    let text = PRE.replace_all(&text, |c: &Captures| {
        pre.push(entities(&TAG.replace_all(&BR.replace_all(&c[1], "\n"), "")));
        format!("\n\x00{}\x00\n", pre.len() - 1)
    });
    // A table's cells and a definition list's terms apart: "1.2", "18.4" ran together as 1.218.4 (#120).
    let text = CELL_END.replace_all(&text, " | ");
    let text = BREAK.replace_all(&text, "\n");
    let text = LI.replace_all(&text, "- ");
    let text = TAG.replace_all(&text, "");
    let text = entities(&text);
    let text = SPACES.replace_all(&text, " ");
    let text = ROW_END.replace_all(&text, ""); // after a row's last cell
    let text = EMPTY_BULLETS.replace_all(&text, ""); // the bullets of emptied list items
    let text = BLANK_LINES.replace_all(&text, "\n\n");
    let text = PRE_MARK
        .replace_all(js_trim(&text), |c: &Captures| {
            match c[1].parse::<usize>().ok().and_then(|i| pre.get(i)) {
                Some(p) => {
                    let p = p.strip_prefix('\n').unwrap_or(p);
                    p.strip_suffix('\n').unwrap_or(p).to_string()
                }
                None => c[0].to_string(),
            }
        })
        .into_owned();
    HtmlText { text, links }
}

/// Links listed under one part of a page at most (a site's navigation can have hundreds).
const MAX_LINKS: usize = 200;

regex!(REFERENCE, r"\]\[(\d+)\]");

/// The link list for the references that appear in `text` ("[3]: https://…"), in order.
fn link_list(text: &str, links: &[String]) -> Option<String> {
    let mut shown: Vec<usize> = REFERENCE
        .captures_iter(text)
        .filter_map(|c| c[1].parse::<usize>().ok())
        .filter(|n| *n <= links.len())
        .collect();
    shown.sort_unstable();
    shown.dedup();
    if shown.is_empty() {
        return None;
    }
    let more = shown.len().saturating_sub(MAX_LINKS);
    let mut lines: Vec<String> = shown
        .iter()
        .take(MAX_LINKS)
        .map(|n| {
            format!(
                "[{n}]: {}",
                n.checked_sub(1)
                    .and_then(|i| links.get(i))
                    .map_or("undefined", String::as_str)
            )
        })
        .collect();
    if more > 0 {
        lines.push(format!("({more} more links not listed)"));
    }
    Some(lines.join("\n"))
}

/// Send `req`, giving up when `signal` is cancelled or `timeout` passes (`AbortSignal.any`).
pub(crate) async fn send_within(
    req: reqwest::RequestBuilder,
    signal: &CancellationToken,
    timeout: Duration,
) -> Result<reqwest::Response, String> {
    tokio::select! {
        _ = signal.cancelled() => Err("The operation was aborted.".into()),
        r = tokio::time::timeout(timeout, req.send()) => match r {
            Err(_) => Err("The operation timed out.".into()),
            Ok(Err(e)) => Err(error_text(&e)),
            Ok(Ok(res)) => Ok(res),
        },
    }
}

/// A reqwest error with its causes, as one line.
pub(crate) fn error_text(e: &reqwest::Error) -> String {
    let mut text = e.to_string();
    let mut source = std::error::Error::source(e);
    while let Some(s) = source {
        text.push_str(&format!(": {s}"));
        source = s.source();
    }
    text
}

pub(crate) fn content_type(res: &reqwest::Response) -> String {
    res.headers()
        .get(reqwest::header::CONTENT_TYPE)
        .map(|v| String::from_utf8_lossy(v.as_bytes()).into_owned())
        .unwrap_or_default()
}

/// Whether `url` answers with a document (a quick look, the body is not read). It is on a site this
/// fetch already reached, but the page's host chose it for the agent, so it is judged like any other
/// URL: its address included.
async fn exists(url: &str, signal: &CancellationToken) -> bool {
    exists_via(&SYSTEM, url, signal).await
}

async fn exists_via(route: &Route, url: &str, signal: &CancellationToken) -> bool {
    let Ok(url) = url::Url::parse(url) else {
        return false;
    };
    let headers = [("user-agent", USER_AGENT)];
    match get_following(
        url,
        Check::All,
        MAX_REDIRECTS,
        route,
        &headers,
        signal,
        Limit::Total(Duration::from_secs(5)),
    )
    .await
    {
        Ok(found) => found.res.status().is_success() && !content_type(&found.res).contains("html"),
        Err(_) => false,
    }
}

fn arg_str(args: &sasacode_ai::JsonObject, key: &str) -> String {
    args.get(key).and_then(Value::as_str).unwrap_or_default().to_string()
}

fn arg_usize(args: &sasacode_ai::JsonObject, key: &str) -> Option<usize> {
    args.get(key).and_then(Value::as_f64).map(|n| n.max(0.0) as usize)
}

async fn fetch_page(
    api: PluginApi,
    args: sasacode_ai::JsonObject,
    signal: CancellationToken,
) -> anyhow::Result<ToolResult> {
    let raw_url = arg_str(&args, "url");
    let Ok(url) = url::Url::parse(&raw_url) else {
        return Ok(error_result(format!("Invalid URL: {raw_url}")));
    };
    if url.scheme() != "http" && url.scheme() != "https" {
        return Ok(error_result("Only http and https URLs are supported."));
    }
    // A URL the agent made up is the usual reason a fetch fails: point it at a way to find the right one.
    let search = api
        .agent()
        .tools()
        .iter()
        .find(|t| ["web_search", "search", "web"].contains(&t.name()))
        .map(|t| t.name().to_string());
    let instead = match &search {
        Some(s) => format!("search with {s}"),
        None => "start from a page you have fetched and follow its links".to_string(),
    };
    let deadline = Duration::from_secs(30);
    let limit = Limit::Total(deadline);
    let headers = [
        ("user-agent", USER_AGENT),
        ("accept", "text/html,text/plain,application/json,*/*"),
    ];
    // The URL the user was asked about is fetched as it is (a dev server on this machine is fine); each
    // redirect is a new place, judged and connected to the address that was judged.
    let followed = match get_following(url, Check::Redirects, MAX_REDIRECTS, &SYSTEM, &headers, &signal, limit).await {
        Ok(followed) => followed,
        Err(e) => {
            let hint = if signal.is_cancelled() || e.refused {
                String::new()
            } else {
                format!("\n[Do not guess another URL: {instead}.]")
            };
            return Ok(error_result(format!("Fetch failed: {}{hint}", e.message)));
        }
    };
    let (res, started) = (followed.res, followed.started);
    let type_ = content_type(&res);
    let status = res.status();
    let final_url = followed.url.to_string();
    // Where the page really came from, said when it is not where the URL pointed.
    let via = followed
        .redirected_from
        .map(|from| format!(" (redirected from {from})"))
        .unwrap_or_default();
    let texty = ["text", "json", "xml", "javascript"].iter().any(|k| type_.contains(k));
    if !texty && !type_.is_empty() {
        return Ok(error_result(format!(
            "{} {type_}: not a text document.{via}",
            status.as_u16()
        )));
    }
    // The body is read under the same signal and timeout as the request.
    let read = tokio::select! {
        _ = signal.cancelled() => anyhow::bail!("The operation was aborted."),
        _ = tokio::time::sleep_until(started + deadline) => anyhow::bail!("The operation timed out."),
        r = read_capped(res, MAX_BYTES) => r?,
    };
    let (body, links) = if type_.contains("html") {
        let t = html_to_text(&readable(&read.text, true), Some(&final_url));
        (t.text, t.links)
    } else {
        (read.text, Vec::new())
    };
    let start = arg_usize(&args, "offset").unwrap_or(0);
    let max = arg_usize(&args, "max_chars").unwrap_or(DEFAULT_MAX);
    let slice = utf16_slice(&body, start, start.saturating_add(max));
    let total = utf16_len(&body);
    let cut = if read.cut {
        format!(" — only the first {} MB was read", MAX_BYTES / 1_000_000)
    } else {
        String::new()
    };
    let mut parts = vec![
        format!("[{} {final_url}{via}{cut}]", status.as_u16()),
        slice.to_string(),
    ];
    if let Some(list) = link_list(slice, &links) {
        parts.push(format!("Links:\n{list}"));
    }
    if start.saturating_add(max) < total {
        let shown = start + utf16_len(slice);
        parts.push(format!(
            "[Showing characters {start}-{shown} of {total}. Use offset={shown} to continue.]"
        ));
    }
    if status.as_u16() == 404 || status.as_u16() == 410 {
        let indexes: Vec<String> = ["/llms.txt", "/sitemap.xml"]
            .iter()
            .filter_map(|p| url::Url::parse(&final_url).ok()?.join(p).ok().map(|u| u.to_string()))
            .filter(|u| *u != final_url)
            .collect();
        let checks = indexes.iter().map(|u| {
            let signal = signal.clone();
            async move { exists(u, &signal).await.then(|| u.clone()) }
        });
        let found: Vec<String> = futures::future::join_all(checks).await.into_iter().flatten().collect();
        let index = if found.is_empty() {
            String::new()
        } else {
            format!("fetch {} (the site's own index) or ", found.join(" or "))
        };
        parts.push(format!(
            "[This page does not exist. Do not guess another URL: {index}{instead}.]"
        ));
    }
    Ok(ToolResult {
        content: vec![sasacode_plugin_api::text(parts.join("\n"))],
        is_error: Some(!status.is_success()),
        details: None,
    })
}

/// web_fetch: read a URL as text. Network access, so it goes through approval like bash.
pub fn plugin(api: PluginApi) -> BoxFuture<'static, anyhow::Result<()>> {
    Box::pin(async move {
        let run_api = api.clone();
        let tool = ToolDef::new(
            "web_fetch",
            &format!(
                "Fetch a URL over HTTP(S) and return it as text (HTML is converted to plain text, its links listed as absolute URLs to follow). Returns up to max_chars characters (default {DEFAULT_MAX}) starting at offset."
            ),
            json!({
                "type": "object",
                "properties": {
                    "url": { "type": "string", "description": "http(s) URL" },
                    "offset": { "type": "integer", "minimum": 0, "description": "Character offset to start from (for long pages)" },
                    "max_chars": { "type": "integer", "minimum": 1 },
                },
                "required": ["url"],
                "additionalProperties": false,
            }),
            ToolKind::Other,
            move |args, ctx| fetch_page(run_api.clone(), args, ctx.signal),
        )
        .concurrent(true)
        .match_target(|a| arg_str(a, "url"))
        .summary(|a| arg_str(a, "url"));
        api.register_tool(tool.into_ref());
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A plain-text document on this machine, and how many requests it got.
    async fn text_server() -> (u16, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let hits = std::sync::Arc::new(AtomicUsize::new(0));
        let counted = hits.clone();
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                counted.fetch_add(1, Ordering::SeqCst);
                tokio::spawn(async move {
                    let mut buf = [0u8; 2048];
                    let _ = socket.read(&mut buf).await;
                    let reply = "HTTP/1.1 200 OK\r\ncontent-type: text/plain\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok";
                    let _ = socket.write_all(reply.as_bytes()).await;
                    let _ = socket.shutdown().await;
                });
            }
        });
        (port, hits)
    }

    #[tokio::test]
    async fn the_site_index_check_does_not_connect_to_an_internal_address() {
        let (port, hits) = text_server().await;
        let signal = CancellationToken::new();
        assert!(!exists(&format!("http://127.0.0.1:{port}/llms.txt"), &signal).await);
        assert!(!exists(&format!("http://localhost:{port}/llms.txt"), &signal).await);
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn the_site_index_check_finds_a_document_at_a_public_address() {
        fn public_name(_host: String) -> BoxFuture<'static, Result<Vec<std::net::IpAddr>, String>> {
            Box::pin(async { Ok(vec![std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)]) })
        }
        fn nothing_internal(_: &std::net::IpAddr) -> bool {
            false
        }
        let route = Route {
            lookup: public_name,
            internal: nothing_internal,
        };
        let (port, hits) = text_server().await;
        let signal = CancellationToken::new();
        assert!(exists_via(&route, &format!("http://index.test:{port}/llms.txt"), &signal).await);
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn code_points_and_entities() {
        assert_eq!(code_point("#65").as_deref(), Some("A"));
        assert_eq!(code_point("#x41").as_deref(), Some("A"));
        assert_eq!(code_point("#X41").as_deref(), Some("A"));
        assert_eq!(code_point("#x110000"), None);
        assert_eq!(code_point("#99999999"), None);
        assert_eq!(entities("&auml;&Eacute;&ccedil;&xuml;"), "äÉç&xuml;");
    }

    #[test]
    fn blocks_end_at_their_own_closing_tag() {
        assert_eq!(without_blocks("a<script>x</SCRIPT>b<style>y</style>c"), "abc");
        assert_eq!(without_blocks("a<svg>never closed"), "a<svg>never closed");
        assert_eq!(
            without_blocks("<header>h</header><p>x</p>"),
            "<header>h</header><p>x</p>"
        );
    }
}
