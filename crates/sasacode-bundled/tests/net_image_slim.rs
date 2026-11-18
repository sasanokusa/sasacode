//! packages/bundled/test/image-slim.test.ts: images shrunk (the image crate where the TS had
//! Bun.Image), and old images left out of a request.

mod support_net;

use std::io::Write;
use std::sync::Arc;

use base64::Engine;
use serde_json::json;

use sasacode_ai::{
    AssistantContent, AssistantMessage, ImageContent, Message, ToolCall, ToolResultMessage, UserContent, UserMessage,
};
use sasacode_bundled::image_slim::{Probe, ShrinkSettings, Spot, omit, opaque, probe, shrink};
use sasacode_plugin_api::hooks::{BeforeRequestEvent, ToolResultEvent};
use sasacode_plugin_api::{ToolResult, hook};
use support_net::*;

#[derive(Clone, Copy, PartialEq)]
enum Alpha {
    No,
    Yes,
    /// RGBA, every pixel opaque.
    Opaque,
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xedb8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

fn chunk(kind: &str, data: &[u8]) -> Vec<u8> {
    let mut t = kind.as_bytes().to_vec();
    t.extend_from_slice(data);
    let mut out = (data.len() as u32).to_be_bytes().to_vec();
    out.extend_from_slice(&t);
    out.extend_from_slice(&crc32(&t).to_be_bytes());
    out
}

/// A PNG of w×h noise (so it compresses badly, like a screenshot's detail).
fn png_bytes(w: usize, h: usize, alpha: Alpha) -> Vec<u8> {
    let ch = if alpha == Alpha::No { 3 } else { 4 };
    let row = w * ch + 1;
    let mut raw = vec![0u8; row * h];
    let mut x: u32 = 1;
    for (i, byte) in raw.iter_mut().enumerate() {
        let k = (i % row) as i64 - 1;
        *byte = if k < 0 {
            0
        } else if alpha == Alpha::Opaque && k % 4 == 3 {
            255
        } else {
            x = x.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            (x >> 24) as u8
        };
    }
    let mut ihdr = vec![0u8; 13];
    ihdr[0..4].copy_from_slice(&(w as u32).to_be_bytes());
    ihdr[4..8].copy_from_slice(&(h as u32).to_be_bytes());
    ihdr[8] = 8;
    ihdr[9] = if alpha == Alpha::No { 2 } else { 6 };
    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    z.write_all(&raw).unwrap_or_else(|e| panic!("{e}"));
    let idat = z.finish().unwrap_or_else(|e| panic!("{e}"));
    let mut file = vec![0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];
    file.extend(chunk("IHDR", &ihdr));
    file.extend(chunk("IDAT", &idat));
    file.extend(chunk("IEND", &[]));
    file
}

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn bytes(data: &str) -> Vec<u8> {
    base64::engine::general_purpose::STANDARD
        .decode(data)
        .unwrap_or_else(|e| panic!("{e}"))
}

fn png(w: usize, h: usize, alpha: Alpha) -> ImageContent {
    ImageContent {
        media_type: "image/png".into(),
        data: Arc::from(b64(&png_bytes(w, h, alpha))),
    }
}

const S: ShrinkSettings = ShrinkSettings {
    max_pixels: 1_048_576.0,
    jpeg_quality: 90,
    min_bytes: 100_000.0,
};

fn probed(data: &str) -> Probe {
    probe(&bytes(data)).unwrap_or_else(|| panic!("not an image"))
}

#[tokio::test]
async fn a_large_screenshot_becomes_a_jpeg_of_about_max_pixels_aspect_ratio_kept_with_a_note_to_scale_positions_back() {
    let r = shrink(&png(1500, 950, Alpha::No).data, S)
        .await
        .unwrap_or_else(|| panic!("not shrunk"));
    assert_eq!(r.media_type, "image/jpeg");
    let p = probed(&r.data);
    assert_eq!(p.kind, "jpeg");
    let area = f64::from(p.w * p.h);
    assert!(area <= S.max_pixels && area > S.max_pixels * 0.99, "{p:?}");
    assert!((f64::from(p.w) / f64::from(p.h) - 1500.0 / 950.0).abs() < 0.01);
    let re = regex::Regex::new(
        r"^1500x950 PNG \d+ KB -> 1286x814 JPEG q90 \d+ KB; multiply positions in it by 1\.17 for the original$",
    )
    .unwrap_or_else(|e| panic!("{e}"));
    assert!(re.is_match(&r.note), "{}", r.note);
}

fn jpeg_of(png: &[u8], quality: u8) -> Vec<u8> {
    let img = image::load_from_memory(png).unwrap_or_else(|e| panic!("{e}")).to_rgb8();
    let mut out = Vec::new();
    let enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality);
    img.write_with_encoder(enc).unwrap_or_else(|e| panic!("{e}"));
    out
}

#[tokio::test]
async fn a_jpeg_turned_by_its_exif_orientation_is_shrunk_upright_its_aspect_ratio_kept() {
    let jpg = jpeg_of(&png_bytes(1600, 1200, Alpha::No), 90);
    // APP1 Exif: a big-endian TIFF whose IFD0 has one entry, Orientation (0x0112) = 6: shown turned 90°, 1200×1600.
    let mut exif = b"Exif\0\0".to_vec();
    exif.extend([
        0x4d, 0x4d, 0, 0x2a, 0, 0, 0, 8, 0, 1, 0x01, 0x12, 0, 3, 0, 0, 0, 1, 0, 6, 0, 0, 0, 0, 0, 0,
    ]);
    let len = exif.len() + 2;
    let mut turned = jpg[..2].to_vec();
    turned.extend([0xff, 0xe1, (len >> 8) as u8, (len & 0xff) as u8]);
    turned.extend(&exif);
    turned.extend(&jpg[2..]);
    let r = shrink(&b64(&turned), S).await.unwrap_or_else(|| panic!("not shrunk"));
    let out = image::load_from_memory(&bytes(&r.data)).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!((out.width(), out.height()), (886, 1182));
    assert!(r.note.starts_with("1200x1600 JPEG"), "{}", r.note);
    assert!(r.note.contains("-> 886x1182 JPEG"), "{}", r.note);
}

#[tokio::test]
async fn within_max_pixels_an_opaque_png_over_min_bytes_becomes_a_jpeg_of_the_same_size_small_transparent_and_jpeg_stay()
 {
    let r = shrink(&png(600, 400, Alpha::No).data, S)
        .await
        .unwrap_or_else(|| panic!("not shrunk"));
    assert_eq!(
        probed(&r.data),
        Probe {
            kind: "jpeg",
            w: 600,
            h: 400,
            alpha: false
        }
    );
    assert!(!r.note.contains("multiply"));
    assert_eq!(shrink(&png(100, 100, Alpha::No).data, S).await, None); // 30 KB
    assert_eq!(shrink(&png(600, 400, Alpha::Yes).data, S).await, None);
    assert_eq!(shrink(&r.data, S).await, None);
    assert_eq!(shrink(&b64(b"not an image"), S).await, None);
}

#[tokio::test]
async fn an_rgba_png_whose_every_pixel_is_opaque_becomes_a_jpeg_one_transparent_pixel_keeps_it_a_png() {
    let shot = png_bytes(600, 400, Alpha::Opaque);
    assert!(probe(&shot).is_some_and(|p| p.alpha));
    assert!(opaque(&shot));
    let r = shrink(&b64(&shot), S).await.unwrap_or_else(|| panic!("not shrunk"));
    assert_eq!(r.media_type, "image/jpeg");
    // Filtered rows (Sub, Up, Average, Paeth), as encoders write them: the copy the image crate makes.
    let img = image::load_from_memory(&shot)
        .unwrap_or_else(|e| panic!("{e}"))
        .to_rgba8();
    let mut filtered = Vec::new();
    img.write_with_encoder(image::codecs::png::PngEncoder::new(&mut filtered))
        .unwrap_or_else(|e| panic!("{e}"));
    assert!(opaque(&filtered));
    assert!(!opaque(&png_bytes(600, 400, Alpha::Yes)));
}

#[tokio::test]
async fn a_large_transparent_png_is_resized_and_stays_a_png() {
    let r = shrink(&png(1600, 1000, Alpha::Yes).data, S)
        .await
        .unwrap_or_else(|| panic!("not shrunk"));
    assert_eq!(r.media_type, "image/png");
    let p = probed(&r.data);
    assert_eq!((p.kind, p.alpha), ("png", true));
    assert_eq!(p.w, 1295);
}

fn spots(n: usize, locked: usize, size: usize) -> Vec<Spot> {
    (0..n)
        .map(|i| Spot {
            key: i.to_string(),
            bytes: size,
            locked: i >= n - locked,
        })
        .collect()
}

#[test]
fn omit_the_oldest_images_go_drop_step_at_a_time_never_the_latest_user_messages_then_by_size() {
    let o = |s: Vec<Spot>| omit(&s, 4, 8, 100);
    assert_eq!(o(spots(11, 0, 1)).len(), 0);
    assert_eq!(
        o(spots(12, 0, 1)).into_iter().collect::<Vec<_>>(),
        ["0", "1", "2", "3", "4", "5", "6", "7"]
    );
    assert_eq!(o(spots(19, 0, 1)).len(), 8); // the prompt's start stays put between steps
    assert_eq!(o(spots(20, 0, 1)).len(), 16);
    assert_eq!(o(spots(12, 6, 1)).len(), 6); // only the droppable ones
    assert_eq!(o(spots(6, 1, 30)).len(), 3); // 180 bytes: the oldest go until 90 is left
}

fn read_call() -> ToolCall {
    ToolCall {
        id: "1".into(),
        name: "read".into(),
        input: Default::default(),
        raw_input: None,
    }
}

async fn tool_result(st: &Setup, result: ToolResult) -> Option<ToolResult> {
    let mut changed = None;
    st.agent
        .hooks()
        .run::<hook::ToolResult>(
            ToolResultEvent {
                call: read_call(),
                result,
                ran: true,
            },
            |r, _, _| {
                if let Some(next) = r.result {
                    changed = Some(next);
                }
                true
            },
        )
        .await;
    changed
}

async fn before_request(st: &Setup, messages: Vec<Message>) -> Vec<Message> {
    let mut sent = messages.clone();
    let event = BeforeRequestEvent {
        messages,
        model: st.agent.model(),
        sampling: Default::default(),
        fetch: None,
    };
    st.agent
        .hooks()
        .run::<hook::BeforeRequest>(event, |r, _, _| {
            if let Some(m) = r.messages {
                sent = m;
            }
            true
        })
        .await;
    sent
}

fn content(m: &Message) -> Vec<UserContent> {
    match m {
        Message::User(u) => u.content.clone(),
        Message::Tool(t) => t.content.clone(),
        Message::Assistant(_) => Vec::new(),
    }
}

fn image_of(c: &UserContent) -> Option<&ImageContent> {
    match c {
        UserContent::Image(i) => Some(i),
        _ => None,
    }
}

fn tool_message(i: usize, content: Vec<UserContent>) -> Message {
    Message::Tool(ToolResultMessage {
        tool_call_id: i.to_string(),
        tool_name: "read".into(),
        content,
        is_error: false,
        timestamp: 0,
    })
}

fn user(content: Vec<UserContent>) -> Message {
    Message::User(UserMessage { content, timestamp: 0 })
}

#[tokio::test]
async fn plugin_a_tools_image_is_stored_shrunk_with_a_note_a_request_sends_the_newest_and_a_note_for_the_others() {
    let root = temp_dir("image-slim-");
    let st = load_plugin("image-slim", bundled("image-slim"), &s(root.path()), json!({})).await;
    let shot = png(1500, 950, Alpha::No);
    let result = ToolResult {
        content: vec![UserContent::text("screenshot"), UserContent::Image(shot.clone())],
        ..ToolResult::default()
    };
    let result = tool_result(&st, result)
        .await
        .unwrap_or_else(|| panic!("left as it was"));
    let kinds: Vec<bool> = result.content.iter().map(|c| image_of(c).is_some()).collect();
    assert_eq!(kinds, [false, true, false]);
    assert_eq!(
        image_of(&result.content[1]).map(|i| i.media_type.as_str()),
        Some("image/jpeg")
    );
    let note = result.content[2].as_text().unwrap_or("");
    assert!(
        note.starts_with("[image-slim: stored as a smaller copy, 1500x950 PNG"),
        "{note}"
    );

    let small = png(10, 10, Alpha::No);
    let mut messages: Vec<Message> = (0..12)
        .map(|i| tool_message(i, vec![UserContent::Image(small.clone())]))
        .collect();
    messages.push(user(vec![
        UserContent::text("and this"),
        UserContent::Image(shot.clone()),
    ]));
    let sent = before_request(&st, messages.clone()).await;
    let images = sent.iter().flat_map(content).filter(|c| image_of(c).is_some()).count();
    assert_eq!(images, 5); // 13 images: the oldest 8 left out
    assert!(sent[..8].iter().all(|m| {
        content(m)
            .first()
            .and_then(|c| c.as_text())
            .is_some_and(|t| t.starts_with("[An image was here."))
    }));
    let last = content(sent.last().unwrap_or_else(|| panic!("nothing sent")));
    assert_eq!(image_of(&last[1]).map(|i| i.media_type.as_str()), Some("image/jpeg")); // the pasted one, shrunk for the request
    let original = content(messages.last().unwrap_or_else(|| panic!("no messages")));
    assert_eq!(image_of(&original[1]), Some(&shot)); // the history is left as it was
}

#[tokio::test]
async fn plugin_a_reminder_after_the_users_message_does_not_take_its_place_the_pasted_images_are_all_sent() {
    let root = temp_dir("image-slim-");
    let st = load_plugin("image-slim", bundled("image-slim"), &s(root.path()), json!({})).await;
    let small = png(10, 10, Alpha::No);
    let mut first = vec![UserContent::text("compare these")];
    first.extend((0..12).map(|_| UserContent::Image(small.clone())));
    let assistant = AssistantMessage {
        content: vec![AssistantContent::ToolCall(read_call())],
        ..reply(Vec::new())
    };
    let messages = vec![
        user(first),
        Message::Assistant(assistant),
        tool_message(1, vec![UserContent::text("ok")]),
        user(vec![UserContent::text(sasacode_host::harness_note(
            "Your last 5 replies were tool calls with no text",
        ))]),
    ];
    let sent = before_request(&st, messages).await;
    assert_eq!(content(&sent[0]).iter().filter(|c| image_of(c).is_some()).count(), 12);
}

#[tokio::test]
async fn plugin_models_limits_it_to_the_models_named() {
    let root = temp_dir("image-slim-");
    let st = load_plugin(
        "image-slim",
        bundled("image-slim"),
        &s(root.path()),
        json!({ "models": ["other"] }),
    )
    .await;
    let shot = png(1500, 950, Alpha::No);
    let result = ToolResult {
        content: vec![UserContent::Image(shot)],
        ..ToolResult::default()
    };
    assert!(tool_result(&st, result).await.is_none());
}
