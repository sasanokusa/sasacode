//! packages/bundled/test/image-attach.test.ts: image paths in a prompt, pasted and dropped images,
//! /image and the clipboard readers (with the program runner replaced).

mod support_net;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use base64::Engine;
use serde_json::{Value, json};

use sasacode_ai::{ImageContent, UserContent};
use sasacode_bundled::image_attach::{
    ClipboardFn, Run, clipboard_image_on, find_refs, plugin_with, resolve_path_in, sniff, words,
};
use sasacode_plugin_api::hooks::UserPromptEvent;
use sasacode_plugin_api::{Lang, hook};
use support_net::*;

const PNG_B64: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";

fn png() -> Vec<u8> {
    base64::engine::general_purpose::STANDARD
        .decode(PNG_B64)
        .unwrap_or_else(|e| panic!("{e}"))
}

fn write(path: &Path, data: &[u8]) {
    std::fs::write(path, data).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
}

/// A project folder: shot.png, "my dir/a b.png", and fake.png that is not an image. Under the temp
/// folder as named (`mkdtempSync(join(tmpdir(), …))`), not its real path.
fn fixture() -> (tempfile::TempDir, PathBuf) {
    let d = temp_dir("image-attach-");
    let cwd = d.path().to_path_buf();
    write(&cwd.join("shot.png"), &png());
    std::fs::create_dir(cwd.join("my dir")).unwrap_or_else(|e| panic!("{e}"));
    write(&cwd.join("my dir").join("a b.png"), &png());
    write(&cwd.join("fake.png"), b"not an image");
    (d, cwd)
}

async fn load(cwd: &Path, settings: Value) -> Setup {
    load_plugin(
        "image-attach",
        sasacode_bundled::image_attach::plugin,
        &s(cwd),
        settings,
    )
    .await
}

/// The prompt as the user_prompt hooks leave it.
async fn prompt_with(st: &Setup, content: Vec<UserContent>) -> (Vec<UserContent>, bool) {
    let mut out = content.clone();
    let mut changed = false;
    st.agent
        .hooks()
        .run::<hook::UserPrompt>(UserPromptEvent { content }, |r, _, _| {
            if let Some(c) = r.content {
                out = c;
                changed = true;
            }
            true
        })
        .await;
    (out, changed)
}

async fn prompt(st: &Setup, input: &str) -> Vec<UserContent> {
    prompt_with(st, vec![UserContent::text(input)]).await.0
}

fn kinds(c: &[UserContent]) -> Vec<&'static str> {
    c.iter()
        .map(|b| {
            if matches!(b, UserContent::Image(_)) {
                "image"
            } else {
                "text"
            }
        })
        .collect()
}

fn text_at(c: &[UserContent], i: usize) -> String {
    c.get(i).and_then(|b| b.as_text()).unwrap_or("").to_string()
}

fn image(media_type: &str, data: &str) -> UserContent {
    UserContent::Image(ImageContent {
        media_type: media_type.into(),
        data: Arc::from(data),
    })
}

/// What a drop pastes: `\ ` for a space, or on Windows the path in quotes.
fn dropped(p: &Path) -> String {
    if cfg!(windows) {
        format!("\"{}\"", p.display())
    } else {
        s(p).replace(' ', "\\ ")
    }
}

#[test]
fn find_refs_at_path_is_explicit_a_bare_path_needs_to_be_absolute_text_after_the_extension_is_left_alone() {
    let p = |t: &str| {
        find_refs(t)
            .into_iter()
            .map(|r| (r.path, r.explicit))
            .collect::<Vec<_>>()
    };
    let v = |items: &[(&str, bool)]| items.iter().map(|(p, e)| (p.to_string(), *e)).collect::<Vec<_>>();
    assert_eq!(p("見て @shot.pngを確認"), v(&[("shot.png", true)]));
    assert_eq!(p("look at /tmp/a\\ b.png now"), v(&[("/tmp/a b.png", false)]));
    assert_eq!(
        p("'/tmp/a b.PNG' and \"~/x.jpg\""),
        v(&[("/tmp/a b.PNG", false), ("~/x.jpg", false)])
    );
    assert_eq!(p("@\"my dir/a b.png\""), v(&[("my dir/a b.png", true)]));
    assert_eq!(p("file:///tmp/x.webp"), v(&[("file:///tmp/x.webp", false)]));
    // Prose that merely mentions a name, an e-mail, a non-image or a URL is not an attachment.
    assert_eq!(
        p("shot.png is bad; a@b.png; /tmp/x.txt; https://x.com/a.png; app.png2"),
        v(&[])
    );
}

#[test]
fn sniff_and_words() {
    assert_eq!(sniff(&png()), Some("image/png"));
    assert_eq!(sniff(b"GIF89a"), Some("image/gif"));
    assert_eq!(sniff(b"RIFF\0\0\0\0WEBPVP8 "), Some("image/webp"));
    assert_eq!(sniff(&[0xff, 0xd8, 0xff, 0xe0]), Some("image/jpeg"));
    assert_eq!(sniff(b"hello"), None);
    assert_eq!(words(r#"a "b c" d\ e 'f g'"#), ["a", "b c", "d e", "f g"]);
}

#[tokio::test]
async fn at_path_and_a_dropped_absolute_path_become_images_after_the_text() {
    let (_d, cwd) = fixture();
    let h = load(&cwd, json!({})).await;
    let abs = cwd.join("my dir").join("a b.png");
    let c = prompt(&h, &format!("これ @shot.pngと {} を比べて", dropped(&abs))).await;
    assert_eq!(kinds(&c), ["text", "image", "image"]);
    assert_eq!(
        text_at(&c, 0),
        format!(
            "これ [Image 1: {}]と [Image 2: {}] を比べて",
            cwd.join("shot.png").display(),
            abs.display()
        )
    );
    assert_eq!(c[1], image("image/png", PNG_B64));
}

#[tokio::test]
async fn tilde_and_file_urls_are_resolved_a_bare_relative_name_is_left_as_text() {
    let (_d, cwd) = fixture();
    let h = load(&cwd, json!({})).await;
    let url = url::Url::from_file_path(cwd.join("shot.png")).unwrap_or_else(|_| panic!("not absolute"));
    assert_eq!(kinds(&prompt(&h, url.as_str()).await), ["text", "image"]);
    assert_eq!(kinds(&prompt(&h, "shot.png はどう？").await), ["text"]);
    // The TS put a file in the real home folder; here the home is another folder (the real one is not touched).
    let home = temp_dir("image-attach-home-");
    let home = home.path();
    assert_eq!(resolve_path_in("~/x.png", &cwd, home), home.join("x.png"));
    assert_eq!(resolve_path_in("~", &cwd, home), home);
}

#[tokio::test]
async fn a_failed_at_is_reported_and_stays_text_a_dropped_non_file_is_silent_fake_images_are_refused() {
    let (_d, cwd) = fixture();
    let h = load(&cwd, json!({})).await;
    assert_eq!(kinds(&prompt(&h, "@missing.png @fake.png").await), ["text"]);
    assert_eq!(h.notes().len(), 2, "{:?}", h.notes());
    let before = h.notes().len();
    assert_eq!(
        kinds(&prompt(&h, &format!("{}/missing.png", cwd.display())).await),
        ["text"]
    );
    assert_eq!(h.notes().len(), before);
}

#[tokio::test]
async fn an_image_over_5_mb_is_refused_with_a_hint() {
    let (_d, cwd) = fixture();
    let h = load(&cwd, json!({})).await;
    let mut big = png();
    big.extend(vec![0u8; 5_000_001]);
    write(&cwd.join("big.png"), &big);
    assert_eq!(kinds(&prompt(&h, "@big.png").await), ["text"]);
    assert!(h.notes().join(",").contains("sips"), "{:?}", h.notes());
}

#[tokio::test]
async fn images_already_in_the_prompt_are_kept_and_no_refs_means_the_hook_does_nothing() {
    let (_d, cwd) = fixture();
    let h = load(&cwd, json!({})).await;
    let existing = image("image/png", "x");
    let (_, changed) = prompt_with(&h, vec![existing.clone(), UserContent::text("plain")]).await;
    assert!(!changed);
    let (content, _) = prompt_with(&h, vec![existing, UserContent::text("@shot.png")]).await;
    assert_eq!(kinds(&content), ["image", "text", "image"]);
}

#[tokio::test]
async fn an_image_label_the_message_already_has_is_not_read_again_and_new_ones_are_numbered_after_it() {
    let (_d, cwd) = fixture();
    let h = load(&cwd, json!({})).await;
    let shot = cwd.join("shot.png");
    let held = image("image/png", "x");
    let (content, _) = prompt_with(
        &h,
        vec![
            UserContent::text(format!("[Image 1: {}] and @shot.png", shot.display())),
            held,
        ],
    )
    .await;
    assert_eq!(kinds(&content), ["text", "image", "image"]);
    assert_eq!(
        text_at(&content, 0),
        format!("[Image 1: {0}] and [Image 2: {0}]", shot.display())
    );
}

#[tokio::test]
async fn the_limit_and_drop_paths_false() {
    let (_d, cwd) = fixture();
    let h = load(&cwd, json!({ "maxImages": 1 })).await;
    assert_eq!(kinds(&prompt(&h, "@shot.png @shot.png").await), ["text", "image"]);
    let off = load(&cwd, json!({ "dropPaths": false })).await;
    assert_eq!(
        kinds(&prompt(&off, &format!("{}/shot.png", cwd.display())).await),
        ["text"]
    );
    assert_eq!(kinds(&prompt(&off, "@shot.png").await), ["text", "image"]);
}

#[tokio::test]
async fn image_queues_an_image_for_the_next_message_only_clear_drops_it() {
    let _l = lang(Lang::En).await;
    let (_d, cwd) = fixture();
    let h = load(&cwd, json!({})).await;
    h.run_command("image", r#"shot.png "my dir/a b.png""#)
        .await
        .unwrap_or_else(|e| panic!("{e:#}"));
    assert_eq!(
        h.host.status().get("image-attach:image-attach").map(String::as_str),
        Some("📎 2")
    );
    let first = prompt(&h, "what?").await;
    assert_eq!(kinds(&first), ["text", "text", "text", "image", "image"]);
    assert!(h.host.status().is_empty(), "{:?}", h.host.status());
    assert_eq!(kinds(&prompt(&h, "again").await), ["text"]);
    h.run_command("image", "shot.png")
        .await
        .unwrap_or_else(|e| panic!("{e:#}"));
    h.run_command("image", "clear")
        .await
        .unwrap_or_else(|e| panic!("{e:#}"));
    assert_eq!(kinds(&prompt(&h, "x").await), ["text"]);
    h.run_command("image", "fake.png")
        .await
        .unwrap_or_else(|e| panic!("{e:#}"));
    assert!(
        h.notes().last().cloned().unwrap_or_default().contains("not a PNG"),
        "{:?}",
        h.notes()
    );
}

#[test]
fn a_macos_screenshot_name_with_a_narrow_no_break_space_before_pm_is_one_path() {
    let name = "/Users/me/Desktop/Screenshot\\ 2026-10-14\\ at\\ 12.34.56\u{202f}PM.png";
    let paths: Vec<String> = find_refs(&format!("{name} これ")).into_iter().map(|r| r.path).collect();
    assert_eq!(
        paths,
        ["/Users/me/Desktop/Screenshot 2026-10-14 at 12.34.56\u{202f}PM.png"]
    );
}

#[tokio::test]
async fn a_pasted_clipboard_image_or_dropped_image_stands_in_the_input_as_image_n_and_goes_with_the_message() {
    let _l = lang(Lang::En).await;
    let (_d, cwd) = fixture();
    let clip = Arc::new(AtomicBool::new(true));
    let c2 = clip.clone();
    let clipboard: ClipboardFn = Arc::new(move || {
        let ok = c2.load(Ordering::SeqCst);
        Box::pin(async move {
            if !ok {
                anyhow::bail!("the clipboard has no image");
            }
            Ok(ImageContent {
                media_type: "image/png".into(),
                data: Arc::from("CLIP"),
            })
        })
    });
    let h = load_plugin(
        "image-attach",
        move |api| plugin_with(api, clipboard),
        &s(&cwd),
        json!({}),
    )
    .await;
    // What the input holds: each claimed paste is added to it, as the TUI does.
    let input = Arc::new(Mutex::new(String::new()));
    let handler = h
        .host
        .paste_handlers()
        .first()
        .cloned()
        .unwrap_or_else(|| panic!("no paste handler"));
    let paste = |text: String| {
        let (handler, input) = (handler.clone(), input.clone());
        async move {
            let now = input.lock().unwrap_or_else(|e| e.into_inner()).clone();
            let out = handler(text, now).await;
            if let Some(o) = &out {
                input.lock().unwrap_or_else(|e| e.into_inner()).push_str(o);
            }
            out
        }
    };
    let some = |s: &str| Some(s.to_string());
    let abs = cwd.join("my dir").join("a b.png");
    let shot = cwd.join("shot.png");
    assert_eq!(paste(String::new()).await, some("[Image #1] ")); // ⌘V of a screenshot, or ctrl+v
    assert_eq!(paste(format!(" {} ", dropped(&abs))).await, some("[Image #2] "));
    assert_eq!(
        paste(format!("{0} {0}", shot.display())).await,
        some("[Image #3] [Image #4] ")
    );
    assert_eq!(paste("just some text".into()).await, None);
    assert_eq!(paste(format!("look at {}", abs.display())).await, None); // not only paths: left to the editor
    assert_eq!(paste(s(&cwd.join("fake.png"))).await, None);
    clip.store(false, Ordering::SeqCst);
    assert_eq!(paste(String::new()).await, None);
    assert!(
        h.notes().last().cloned().unwrap_or_default().contains("no image"),
        "{:?}",
        h.notes()
    );

    // #3 deleted from the input: it is forgotten and the next image takes its number.
    let drop3 = || {
        let mut i = input.lock().unwrap_or_else(|e| e.into_inner());
        *i = i.replacen("[Image #3] ", "", 1);
    };
    drop3();
    clip.store(true, Ordering::SeqCst);
    assert_eq!(paste(String::new()).await, some("[Image #3] "));
    drop3();

    // The marker for #3 was deleted from the input again, so that image is not sent.
    let c = prompt(&h, "[Image #1] と [Image #2] の違いは？ [Image #4] [Image #9]").await;
    assert_eq!(kinds(&c), ["text", "image", "image", "image"]);
    assert_eq!(
        text_at(&c, 0),
        format!(
            "[Image 1: clipboard] と [Image 2: {}] の違いは？ [Image 3: {}] [Image #9]",
            abs.display(),
            shot.display()
        )
    );
    assert_eq!(c[1], image("image/png", "CLIP"));
    // Sent once: the same marker later is just text.
    assert_eq!(kinds(&prompt(&h, "[Image #1]").await), ["text"]);
}

fn run(f: impl Fn(String, Vec<String>) -> anyhow::Result<Vec<u8>> + Send + Sync + 'static) -> Run {
    let f = Arc::new(f);
    Arc::new(move |cmd, args| {
        let r = f(cmd, args);
        Box::pin(async move { r })
    })
}

fn captured(re: &str, text: &str) -> String {
    let re = regex::Regex::new(re).unwrap_or_else(|e| panic!("{e}"));
    re.captures(text)
        .map(|c| c[1].to_string())
        .unwrap_or_else(|| panic!("no match in {text}"))
}

async fn clip_err(platform: &str, exec: Run) -> String {
    clipboard_image_on(platform, exec)
        .await
        .err()
        .map(|e| e.to_string())
        .unwrap_or_default()
}

#[tokio::test]
async fn clipboard_linux_reads_stdout_macos_goes_through_a_file_the_script_writes_empty_is_a_clear_error() {
    let seen: Arc<Mutex<Vec<String>>> = Arc::default();
    let s2 = seen.clone();
    let linux = run(move |cmd, _| {
        s2.lock().unwrap_or_else(|e| e.into_inner()).push(cmd.clone());
        if cmd == "wl-paste" {
            anyhow::bail!("nope");
        }
        Ok(png())
    });
    let r = clipboard_image_on("linux", linux)
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(&*r.data, PNG_B64);
    assert_eq!(*seen.lock().unwrap_or_else(|e| e.into_inner()), ["wl-paste", "xclip"]);
    assert!(clip_err("linux", run(|_, _| Ok(Vec::new()))).await.contains("no image"));
    let mac = run(|_, args| {
        write(
            Path::new(&captured(r#"POSIX file "([^"]+)""#, &args.join("\n"))),
            &png(),
        );
        Ok(Vec::new())
    });
    assert_eq!(
        clipboard_image_on("darwin", mac)
            .await
            .map(|i| i.media_type)
            .ok()
            .as_deref(),
        Some("image/png")
    );
    assert!(
        clip_err("darwin", run(|_, _| anyhow::bail!("Command failed: osascript")))
            .await
            .contains("no image")
    );
    let win = run(|cmd, args| {
        assert_eq!(cmd, "powershell.exe");
        write(
            Path::new(&captured(r"Save\('([^']+)'", &args.join(" ")).replace("''", "'")),
            &png(),
        );
        Ok(Vec::new())
    });
    assert_eq!(
        clipboard_image_on("win32", win)
            .await
            .map(|i| i.media_type)
            .ok()
            .as_deref(),
        Some("image/png")
    );
    assert!(
        clip_err("win32", run(|_, _| anyhow::bail!("Command failed: powershell.exe")))
            .await
            .contains("no image")
    );
    assert!(
        clip_err("freebsd", run(|_, _| Ok(Vec::new())))
            .await
            .contains("not supported")
    );
}

#[tokio::test]
async fn an_apostrophe_in_the_text_does_not_swallow_a_path_and_a_file_url_with_a_host_costs_no_other_image() {
    let (_d, cwd) = fixture();
    let h = load(&cwd, json!({})).await;
    for text in [
        format!("Here's the screenshot {} what's wrong?", cwd.join("shot.png").display()),
        "what's in @shot.png? it's weird".to_string(),
    ] {
        assert_eq!(find_refs(&text).len(), 1, "{text}");
        assert_eq!(kinds(&prompt(&h, &text).await), ["text", "image"], "{text}");
    }
    let paths = |t: &str| find_refs(t).into_iter().map(|r| r.path).collect::<Vec<_>>();
    assert_eq!(paths("it's '/tmp/a b.png', isn't it"), ["/tmp/a b.png"]);
    // Japanese right after the closing quote (#132), but not a word going on in ASCII.
    for text in [
        "@'/tmp/a b.png'を見て",
        "@'/tmp/a b.png'です",
        "「@'/tmp/a b.png'」と書いた",
        "@'/tmp/a b.png'はどう？",
    ] {
        assert_eq!(paths(text), ["/tmp/a b.png"], "{text}");
    }
    assert!(!paths("it's 'a b.png'what's").contains(&"a b.png".to_string()));
    assert_eq!(
        kinds(&prompt(&h, "file://nas.local/share/a.png and @shot.png").await),
        ["text", "image"]
    );
}
