//! image-slim (`packages/bundled/src/image-slim.ts`). Every request carries every image in the
//! history, so a session that reads many screenshots grows past a gateway's body limit (413 at
//! 8 MiB after 20 of them) and pays for them all each time.
//!
//! 1. Shrink, keeping what the model sees: providers scale large images down anyway (Qwen's encoder
//!    to about 1,048,576 px, Anthropic's to about 1.15 MP), so an image over `maxPixels` is resized
//!    to that area, aspect ratio kept. A PNG without transparency over `minBytes` becomes a JPEG
//!    (`jpegQuality`). The original stays when the copy is not smaller. Done once when a tool
//!    returns an image (the session keeps the copy), and in each request for images that came
//!    another way (pasted, older sessions).
//! 2. Leave out old images: only the newest go with a request, older ones become a short note in it
//!    (the session keeps them). The cut moves `dropStep` images at a time, so between steps the start
//!    of the prompt stays the same and the provider's cache holds. Images in the latest user message
//!    always go; past `maxImageBytes` of images, more old ones are left out.
//!
//! Command: /image-slim (what the last request sent).
//! settings: { models (["*"]), maxPixels (1048576), jpegQuality (90), minBytes (100000),
//!   keepImages (4) and dropStep (8): 4 to 11 images go, maxImageBytes (8 MiB of base64) }

use std::io::{Cursor, Read};
use std::sync::{Arc, Mutex};

use base64::Engine;
use futures::future::BoxFuture;
use image::{DynamicImage, ImageDecoder, ImageEncoder};
use indexmap::{IndexMap, IndexSet};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use sasacode_ai::{ImageContent, Message, UserContent};
use sasacode_host::{note_text, t};
use sasacode_plugin_api::{BeforeRequestResult, Command, NotifyLevel, PluginApi, ToolResultResult, hook};

use crate::net_util::{decode_base64, js_num_str, js_round, to_fixed};

const OMITTED: &str = "[An image was here. It is left out of this request to keep the request small. Read the file again (or ask the user to paste it again) if you need to see it.]";

/// What [`probe`] reads from an image's header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Probe {
    /// "png" or "jpeg".
    pub kind: &'static str,
    pub w: u32,
    pub h: u32,
    pub alpha: bool,
}

/// A smaller copy of an image and what was done to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shrunk {
    pub media_type: String,
    pub data: String,
    pub note: String,
}

fn u32_be(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(at..at + 4)?.try_into().ok()?))
}
fn u16_be(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

/// Width, height and transparency from the header, without decoding the image.
pub fn probe(b: &[u8]) -> Option<Probe> {
    if b.len() > 33 && u32_be(b, 0) == Some(0x8950_4e47) && b.get(12..16) == Some(b"IHDR".as_slice()) {
        let mut alpha = b[25] == 4 || b[25] == 6;
        // A tRNS chunk before the image data also means transparency.
        let mut i = 8usize;
        while !alpha && i + 8 <= b.len() {
            let kind = &b[i + 4..i + 8];
            if kind == b"tRNS" {
                alpha = true;
            }
            if kind == b"IDAT" || kind == b"IEND" {
                break;
            }
            let Some(len) = u32_be(b, i) else { break };
            i += 12 + len as usize;
        }
        return Some(Probe {
            kind: "png",
            w: u32_be(b, 16)?,
            h: u32_be(b, 20)?,
            alpha,
        });
    }
    if b.len() > 4 && b[0] == 0xff && b[1] == 0xd8 {
        let mut i = 2usize;
        while i + 9 < b.len() {
            let m = b[i + 1];
            if b[i] != 0xff || m == 0xff {
                i += 1;
                continue;
            }
            if m == 0x01 || (0xd0..=0xd8).contains(&m) {
                i += 2;
                continue;
            }
            // A start-of-frame marker (not DHT, JPG, DAC) holds the size.
            if (0xc0..=0xcf).contains(&m) && m != 0xc4 && m != 0xc8 && m != 0xcc {
                return Some(Probe {
                    kind: "jpeg",
                    w: u32::from(u16_be(b, i + 7)?),
                    h: u32::from(u16_be(b, i + 5)?),
                    alpha: false,
                });
            }
            i += 2 + usize::from(u16_be(b, i + 2)?);
        }
    }
    None
}

/// Whether a PNG whose format has an alpha channel (8-bit gray or RGB with alpha, not interlaced) uses
/// it nowhere: macOS saves every screenshot so, and such a file can go as a JPEG.
pub fn opaque(b: &[u8]) -> bool {
    if b.len() < 29 || b[24] != 8 || (b[25] != 4 && b[25] != 6) || b[28] != 0 {
        return false;
    }
    let mut idat: Vec<u8> = Vec::new();
    let mut i = 8usize;
    while i + 8 <= b.len() {
        let Some(len) = u32_be(b, i).map(|l| l as usize) else {
            break;
        };
        let kind = &b[i + 4..i + 8];
        if kind == b"tRNS" {
            return false;
        }
        if kind == b"IDAT" {
            let end = (i + 8 + len).min(b.len());
            idat.extend_from_slice(&b[i + 8..end]);
        }
        i += 12 + len;
    }
    let mut data = Vec::new();
    if flate2::read::ZlibDecoder::new(idat.as_slice())
        .read_to_end(&mut data)
        .is_err()
    {
        return false;
    }
    let bpp = if b[25] == 6 { 4 } else { 2 };
    let (Some(w), Some(h)) = (u32_be(b, 16), u32_be(b, 20)) else {
        return false;
    };
    let row = w as usize * bpp;
    let h = h as usize;
    if data.len() < (row + 1) * h {
        return false;
    }
    let (mut prev, mut cur) = (vec![0u8; row], vec![0u8; row]);
    // Undo each row's filter (PNG spec 9.2), then look at every pixel's last byte.
    for y in 0..h {
        let at = y * (row + 1) + 1;
        let f = data[at - 1];
        for i in 0..row {
            let x = data[at + i];
            let a = if i >= bpp { cur[i - bpp] } else { 0 };
            let up = prev[i];
            let c = if i >= bpp { prev[i - bpp] } else { 0 };
            cur[i] = match f {
                0 => x,
                1 => x.wrapping_add(a),
                2 => x.wrapping_add(up),
                3 => x.wrapping_add(((u16::from(a) + u16::from(up)) >> 1) as u8),
                4 => {
                    let (ai, ui, ci) = (i32::from(a), i32::from(up), i32::from(c));
                    let (pa, pb, pc) = ((ui - ci).abs(), (ai - ci).abs(), (ai + ui - 2 * ci).abs());
                    x.wrapping_add(if pa <= pb && pa <= pc {
                        a
                    } else if pb <= pc {
                        up
                    } else {
                        c
                    })
                }
                _ => return false,
            };
        }
        if (bpp - 1..row).step_by(bpp).any(|i| cur[i] != 255) {
            return false;
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    true
}

fn kb(base64: f64) -> String {
    format!("{} KB", js_num_str(js_round(base64 * 3.0 / 4.0 / 1024.0)))
}

/// What [`shrink`] does: the area images are scaled to, the JPEG quality, the smallest PNG turned into a JPEG.
#[derive(Debug, Clone, Copy)]
pub struct ShrinkSettings {
    pub max_pixels: f64,
    pub jpeg_quality: u8,
    pub min_bytes: f64,
}

/// The decoded image, turned upright by its EXIF orientation.
fn decode_upright(bytes: &[u8]) -> Option<DynamicImage> {
    let mut decoder = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()?
        .into_decoder()
        .ok()?;
    let orientation = decoder.orientation().ok();
    let mut img = DynamicImage::from_decoder(decoder).ok()?;
    if let Some(o) = orientation {
        img.apply_orientation(o);
    }
    Some(img)
}

/// A smaller copy of the image (base64 `data`), or None to send it as it is.
pub async fn shrink(data: &str, s: ShrinkSettings) -> Option<Shrunk> {
    let data = data.to_string();
    tokio::task::spawn_blocking(move || shrink_now(&data, s))
        .await
        .ok()
        .flatten()
}

fn shrink_now(data: &str, s: ShrinkSettings) -> Option<Shrunk> {
    // The header (and a JPEG's frame after its EXIF) is near the start: decode only that much to decide.
    let head = data.get(..349_528.min(data.len())).unwrap_or(data);
    let p = probe(&decode_base64(head))?;
    if p.w == 0 || p.h == 0 {
        return None;
    }
    let big = f64::from(p.w) * f64::from(p.h) > s.max_pixels;
    if !big && (p.kind == "jpeg" || (data.len() as f64 * 3.0 / 4.0) < s.min_bytes) {
        return None;
    }
    let original = decode_base64(data);
    let alpha = p.alpha && !opaque(&original);
    if !big && alpha {
        return None;
    }
    let jpeg = !alpha;
    // As decoded: turned by a JPEG's EXIF orientation, where 5 to 8 swap width and height (a phone's
    // portrait photo). Sized from the frame's width and height, the portrait was squeezed into a
    // landscape box (#93).
    let mut img = decode_upright(&original)?; // a damaged or unusual file goes as it is
    let (big_w, big_h) = (img.width(), img.height());
    let (mut w, mut h) = (big_w, big_h);
    if big {
        let scale = (s.max_pixels / (f64::from(big_w) * f64::from(big_h))).sqrt();
        w = ((f64::from(big_w) * scale).floor() as u32).max(1);
        h = ((f64::from(big_h) * scale).floor() as u32).max(1);
        img = img.resize_exact(w, h, image::imageops::FilterType::CatmullRom);
    }
    let mut out: Vec<u8> = Vec::new();
    let encoded = if jpeg {
        let rgb = img.to_rgb8();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, s.jpeg_quality).encode_image(&rgb)
    } else {
        let rgba = img.to_rgba8();
        image::codecs::png::PngEncoder::new(&mut out).write_image(
            rgba.as_raw(),
            rgba.width(),
            rgba.height(),
            image::ExtendedColorType::Rgba8,
        )
    };
    if encoded.is_err() || out.len() >= original.len() {
        return None;
    }
    let format = if jpeg {
        format!("JPEG q{}", s.jpeg_quality)
    } else {
        "PNG".to_string()
    };
    let scaled = if big {
        format!(
            "; multiply positions in it by {} for the original",
            to_fixed(f64::from(big_w) / f64::from(w), 2)
        )
    } else {
        String::new()
    };
    Some(Shrunk {
        media_type: if jpeg { "image/jpeg" } else { "image/png" }.to_string(),
        data: base64::engine::general_purpose::STANDARD.encode(&out),
        note: format!(
            "{big_w}x{big_h} {} {} -> {w}x{h} {format} {}{scaled}",
            p.kind.to_uppercase(),
            kb(data.len() as f64),
            kb(out.len() as f64 * 4.0 / 3.0)
        ),
    })
}

/// One image in a request: `message:block`, its base64 size, and whether it must go.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spot {
    pub key: String,
    pub bytes: usize,
    pub locked: bool,
}

/// Which images (`message:block`) to leave out: the oldest, `drop_step` at a time, never the latest user message's.
pub fn omit(spots: &[Spot], keep_images: usize, drop_step: usize, max_image_bytes: usize) -> IndexSet<String> {
    let droppable: Vec<&Spot> = spots.iter().filter(|sp| !sp.locked).collect();
    let over = spots.len().saturating_sub(keep_images);
    let step = drop_step.max(1);
    let mut drop = ((over / step) * step).min(droppable.len());
    let total: usize = spots.iter().map(|sp| sp.bytes).sum();
    let mut bytes = total - droppable[..drop].iter().map(|sp| sp.bytes).sum::<usize>();
    while bytes > max_image_bytes && drop < droppable.len() {
        bytes -= droppable[drop].bytes;
        drop += 1;
    }
    droppable[..drop].iter().map(|sp| sp.key.clone()).collect()
}

struct Slim {
    models: Vec<String>,
    shrink: ShrinkSettings,
    keep_images: usize,
    drop_step: usize,
    max_image_bytes: usize,
    /// Each request shrinks the same images again: the answer by content.
    cache: Mutex<IndexMap<String, Option<Shrunk>>>,
    last: Mutex<Option<(usize, usize, usize)>>,
    stored: Mutex<(usize, usize, usize)>,
}

impl Slim {
    fn targets(&self, id: &str) -> bool {
        !id.is_empty() && self.models.iter().any(|m| m == "*" || m == id)
    }

    async fn cached(&self, data: &str) -> Option<Shrunk> {
        let key: String = Sha256::digest(data.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let known = self.cache.lock().unwrap_or_else(|e| e.into_inner()).get(&key).cloned();
        if let Some(known) = known {
            return known;
        }
        let shrunk = shrink(data, self.shrink).await;
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        cache.insert(key, shrunk.clone());
        if cache.len() > 256 {
            cache.shift_remove_index(0);
        }
        shrunk
    }
}

fn image(media_type: &str, data: &str) -> UserContent {
    UserContent::Image(ImageContent {
        media_type: media_type.to_string(),
        data: Arc::from(data),
    })
}

fn text(t: &str) -> UserContent {
    UserContent::text(t)
}

fn content_of(m: &Message) -> Option<&Vec<UserContent>> {
    match m {
        Message::User(u) => Some(&u.content),
        Message::Tool(t) => Some(&t.content),
        Message::Assistant(_) => None,
    }
}

pub fn plugin(api: PluginApi) -> BoxFuture<'static, anyhow::Result<()>> {
    Box::pin(async move {
        let s = api.define_settings(
            &json!({
                "type": "object",
                "properties": {
                    "models": { "type": "array", "items": { "type": "string", "minLength": 1 } },
                    "maxPixels": { "type": "integer", "minimum": 65536 },
                    "jpegQuality": { "type": "integer", "minimum": 50, "maximum": 100 },
                    "minBytes": { "type": "integer", "minimum": 0 },
                    "keepImages": { "type": "integer", "minimum": 1 },
                    "dropStep": { "type": "integer", "minimum": 1 },
                    "maxImageBytes": { "type": "integer", "minimum": 1 },
                },
                "additionalProperties": false,
            }),
            Some(&json!({
                "models": ["*"],
                "maxPixels": 1_048_576,
                "jpegQuality": 90,
                "minBytes": 100_000,
                "keepImages": 4,
                "dropStep": 8,
                "maxImageBytes": 8 * 1024 * 1024,
            })),
        )?;
        let n = |k: &str| s.get(k).and_then(Value::as_f64).unwrap_or(0.0);
        let slim = Arc::new(Slim {
            models: s
                .get("models")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(|m| m.as_str().map(str::to_string)).collect())
                .unwrap_or_default(),
            shrink: ShrinkSettings {
                max_pixels: n("maxPixels"),
                jpeg_quality: n("jpegQuality").clamp(1.0, 100.0) as u8,
                min_bytes: n("minBytes"),
            },
            keep_images: n("keepImages") as usize,
            drop_step: n("dropStep") as usize,
            max_image_bytes: n("maxImageBytes") as usize,
            cache: Mutex::new(IndexMap::new()),
            last: Mutex::new(None),
            stored: Mutex::new((0, 0, 0)),
        });

        let on_result = slim.clone();
        let model_api = api.clone();
        api.on::<hook::ToolResult, _, _>(move |e, _ctx| {
            let slim = on_result.clone();
            let model = model_api.agent().model().id;
            async move {
                let has_image = e.result.content.iter().any(|c| matches!(c, UserContent::Image(_)));
                if !slim.targets(&model) || !has_image {
                    return Ok(None);
                }
                let mut content = Vec::new();
                let mut changed = false;
                for c in &e.result.content {
                    let UserContent::Image(img) = c else {
                        content.push(c.clone());
                        continue;
                    };
                    let Some(r) = slim.cached(&img.data).await else {
                        content.push(c.clone());
                        continue;
                    };
                    changed = true;
                    {
                        let mut stored = slim.stored.lock().unwrap_or_else(|p| p.into_inner());
                        stored.0 += 1;
                        stored.1 += img.data.len();
                        stored.2 += r.data.len();
                    }
                    content.push(image(&r.media_type, &r.data));
                    content.push(text(&format!("[image-slim: stored as a smaller copy, {}]", r.note)));
                }
                if !changed {
                    return Ok(None);
                }
                let mut result = e.result;
                result.content = content;
                Ok(Some(ToolResultResult { result: Some(result) }))
            }
        });

        let on_request = slim.clone();
        api.on::<hook::BeforeRequest, _, _>(move |e, _ctx| {
            let slim = on_request.clone();
            async move {
                if !slim.targets(&e.model.id) {
                    return Ok(None);
                }
                // The user's: a reminder (todo, progress-notes) is a user turn too, and took the pasted images' place (#94).
                let last_user = e
                    .messages
                    .iter()
                    .rposition(|m| matches!(m, Message::User(u) if note_text(&u.content).is_none()));
                let mut spots: Vec<Spot> = Vec::new();
                let mut replaced: IndexMap<String, UserContent> = IndexMap::new();
                for (mi, m) in e.messages.iter().enumerate() {
                    let Some(content) = content_of(m) else { continue };
                    for (ci, c) in content.iter().enumerate() {
                        let UserContent::Image(img) = c else { continue };
                        let key = format!("{mi}:{ci}");
                        let r = slim.cached(&img.data).await;
                        let bytes = r.as_ref().map(|r| r.data.len()).unwrap_or(img.data.len());
                        if let Some(r) = r {
                            replaced.insert(key.clone(), image(&r.media_type, &r.data));
                        }
                        spots.push(Spot {
                            key,
                            bytes,
                            locked: Some(mi) == last_user,
                        });
                    }
                }
                if spots.is_empty() {
                    *slim.last.lock().unwrap_or_else(|p| p.into_inner()) = None;
                    return Ok(None);
                }
                let omitted = omit(&spots, slim.keep_images, slim.drop_step, slim.max_image_bytes);
                let sent_bytes = spots
                    .iter()
                    .filter(|sp| !omitted.contains(&sp.key))
                    .map(|sp| sp.bytes)
                    .sum();
                *slim.last.lock().unwrap_or_else(|p| p.into_inner()) =
                    Some((spots.len(), spots.len() - omitted.len(), sent_bytes));
                if replaced.is_empty() && omitted.is_empty() {
                    return Ok(None);
                }
                let next: Vec<Message> = e
                    .messages
                    .iter()
                    .enumerate()
                    .map(|(mi, m)| {
                        let swap = |content: &[UserContent]| -> Vec<UserContent> {
                            content
                                .iter()
                                .enumerate()
                                .map(|(ci, c)| {
                                    let key = format!("{mi}:{ci}");
                                    if omitted.contains(&key) {
                                        text(OMITTED)
                                    } else {
                                        replaced.get(&key).cloned().unwrap_or_else(|| c.clone())
                                    }
                                })
                                .collect()
                        };
                        match m {
                            Message::User(u) => {
                                let mut u = u.clone();
                                u.content = swap(&u.content);
                                Message::User(u)
                            }
                            Message::Tool(t) => {
                                let mut t = t.clone();
                                t.content = swap(&t.content);
                                Message::Tool(t)
                            }
                            Message::Assistant(_) => m.clone(),
                        }
                    })
                    .collect();
                Ok(Some(BeforeRequestResult {
                    messages: Some(next),
                    ..BeforeRequestResult::default()
                }))
            }
        });

        let shown = slim.clone();
        let run_api = api.clone();
        api.register_command(
            Command::new("image-slim", &t!("画像を縮め、古い画像を送らない処理の状態を出す"), move |_ctx| {
                let slim = shown.clone();
                let api = run_api.clone();
                async move {
                    let model = api.agent().model().id;
                    let id = if model.is_empty() { "?".to_string() } else { model };
                    let mut lines = vec![if slim.targets(&id) {
                        t!("いまのモデル（{id}）に効きます。", id = id)
                    } else {
                        t!("いまのモデル（{id}）には効きません（対象: {models}）。", id = id, models = slim.models.join(", "))
                    }];
                    if let Some((total, sent, bytes)) = *slim.last.lock().unwrap_or_else(|p| p.into_inner()) {
                        lines.push(t!(
                            "直近のリクエスト: 画像{total}枚のうち{sent}枚を送り、{omitted}枚は省きました（送った画像は計{size}）。",
                            total = total,
                            sent = sent,
                            omitted = total - sent,
                            size = kb(bytes as f64)
                        ));
                    }
                    let (count, before, after) = *slim.stored.lock().unwrap_or_else(|p| p.into_inner());
                    if count > 0 {
                        lines.push(t!(
                            "読んだ画像を縮めた数: {count}枚（{before} → {after}）",
                            count = count,
                            before = kb(before as f64),
                            after = kb(after as f64)
                        ));
                    }
                    api.ui().notify(&lines.join("\n"), NotifyLevel::Info);
                    Ok(())
                }
            })
            .complete(|_prefix: String| async { Vec::new() }),
        );
        Ok(())
    })
}
