//! The line under the conversation while a run is going: what it is doing, for how long, and how
//! many tokens the model has produced so far (counted up as they stream in), and how fast
//! (`activity.ts`). The app calls [`Activity::tick`] every [`TICK_MS`].

use sasacode_host::{Lang, current_lang, t};

use crate::jsfmt::{js_round, to_fixed};
use crate::ui::{Component, Text};

const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
pub const TICK_MS: u64 = 80;

/// Shown while the model works: one per reply, never the same one twice in a row.
pub const PHRASES_JA: [&str; 22] = [
    "考え中…",
    "思案中…",
    "笹の葉を数え中…",
    "パンダと相談中…",
    "ラバーダックに説明中…",
    "脳内会議中…",
    "行間を読み中…",
    "仮説を煮込み中…",
    "アイデアを発酵中…",
    "ロジックを編み中…",
    "バグの気配を嗅ぎ中…",
    "セミコロンと交渉中…",
    "型とにらめっこ中…",
    "知恵を絞り中…",
    "ひらめき待ち…",
    "思考の竹林を散策中…",
    "笹舟を浮かべ中…",
    "点と点をつなぎ中…",
    "ああでもないこうでもない中…",
    "キーボードを温め中…",
    "沈思黙考中…",
    "スタックトレースを占い中…",
];

pub const PHRASES_EN: [&str; 22] = [
    "Thinking…",
    "Pondering…",
    "Counting bamboo leaves…",
    "Asking the panda…",
    "Explaining it to the rubber duck…",
    "Holding a meeting in my head…",
    "Reading between the lines…",
    "Simmering a hypothesis…",
    "Fermenting ideas…",
    "Weaving logic…",
    "Sniffing out bugs…",
    "Negotiating with semicolons…",
    "Staring down the types…",
    "Racking my brain…",
    "Waiting for a eureka…",
    "Strolling through the bamboo grove…",
    "Floating a bamboo boat…",
    "Connecting the dots…",
    "Going back and forth…",
    "Warming up the keyboard…",
    "Mulling it over…",
    "Reading the stack trace's tea leaves…",
];

pub fn phrases(lang: Lang) -> &'static [&'static str] {
    match lang {
        Lang::Ja => &PHRASES_JA,
        Lang::En => &PHRASES_EN,
    }
}

/// Tokens in streamed text, roughly: about 4 characters each in English, fewer in Japanese
/// (characters as JS counts them: UTF-16 units).
pub fn estimate_tokens(s: &str) -> f64 {
    let mut ascii = 0usize;
    let mut other = 0usize;
    for c in s.chars() {
        if (c as u32) < 128 {
            ascii += 1;
        } else {
            other += c.len_utf16();
        }
    }
    ascii as f64 / 4.0 + other as f64 / 1.3
}

pub fn fmt_count(n: u64) -> String {
    if n < 1000 {
        return n.to_string();
    }
    if n < 10_000 {
        format!("{}k", to_fixed(n as f64 / 1000.0, 1))
    } else {
        format!("{}k", js_round(n as f64 / 1000.0))
    }
}

/// A speed measured over less than this would be a burst of buffered chunks, not the model's pace.
const RATE_MIN_MS: i64 = 500;

pub fn fmt_rate(per_second: f64) -> String {
    if per_second < 10.0 {
        to_fixed(per_second, 1)
    } else {
        format!("{}", js_round(per_second))
    }
}

fn fmt_elapsed(ms: i64) -> String {
    let s = ms.max(0) / 1000;
    if s < 60 {
        t("{s}秒", &[("s", &s)])
    } else {
        t("{m}分{s}秒", &[("m", &(s / 60)), ("s", &(s % 60))])
    }
}

pub type Clock = Box<dyn Fn() -> i64 + Send>;
pub type Random = Box<dyn FnMut() -> f64 + Send>;

pub struct Activity {
    text: Text,
    frame: usize,
    started: i64,
    phrase: String,
    replies: usize,
    detail: Option<String>,
    /// Output tokens of the replies finished in this run, as the provider reported them.
    settled: f64,
    /// The reply streaming now, estimated from its text.
    streaming: f64,
    /// What the line shows: it climbs toward settled + streaming a step each tick.
    shown: u64,
    /// When the reply streaming now sent its first and its latest delta.
    first_at: Option<i64>,
    last_at: i64,
    /// Tokens a second of the reply streaming now, or of the last one while tools run.
    rate: Option<f64>,
    spinner_color: fn(&str) -> String,
    text_color: fn(&str) -> String,
    random: Random,
    now: Clock,
}

impl Activity {
    pub fn new(spinner_color: fn(&str) -> String, text_color: fn(&str) -> String, random: Random, now: Clock) -> Self {
        let started = now();
        let mut a = Self {
            text: Text::new("", 1, 0),
            frame: 0,
            started,
            phrase: String::new(),
            replies: 0,
            detail: None,
            settled: 0.0,
            streaming: 0.0,
            shown: 0,
            first_at: None,
            last_at: 0,
            rate: None,
            spinner_color,
            text_color,
            random,
            now,
        };
        a.next_phrase();
        a.tick();
        a
    }

    /// The real clock and a pseudo-random phrase order.
    pub fn with_defaults(spinner_color: fn(&str) -> String, text_color: fn(&str) -> String) -> Self {
        let mut seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(1)
            | 1;
        let random: Random = Box::new(move || {
            // xorshift64*: enough to vary the phrase.
            seed ^= seed >> 12;
            seed ^= seed << 25;
            seed ^= seed >> 27;
            (seed.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11) as f64 / (1u64 << 53) as f64
        });
        Self::new(spinner_color, text_color, random, Box::new(sasacode_ai::now_ms))
    }

    /// A reply starts streaming: back from a tool or a check, with a new phrase after the first.
    pub fn new_reply(&mut self) {
        if self.replies > 0 {
            self.next_phrase();
        }
        self.replies += 1;
        self.detail = None;
        self.first_at = None;
    }

    /// A reply thrown away (cut off, to be asked for again): its tokens no longer count.
    pub fn discard_reply(&mut self) {
        self.streaming = 0.0;
        self.first_at = None;
    }

    fn next_phrase(&mut self) {
        let list = phrases(current_lang());
        let mut next = self.phrase.clone();
        let mut i = 0;
        while i < 5 && next == self.phrase {
            let k = ((self.random)() * list.len() as f64).floor() as usize;
            next = list[k.min(list.len() - 1)].to_string();
            i += 1;
        }
        self.phrase = next;
    }

    /// What the run waits on besides the model (a permission check, a tool); None for the model.
    pub fn set_detail(&mut self, text: Option<String>) {
        self.detail = text;
        self.update();
    }

    pub fn streamed(&mut self, delta: &str) {
        self.streaming += estimate_tokens(delta);
        self.last_at = (self.now)();
        let first = *self.first_at.get_or_insert(self.last_at);
        // From the first delta on: the wait for it is the prompt being read, not output.
        if self.last_at - first >= RATE_MIN_MS {
            self.rate = Some(self.streaming * 1000.0 / (self.last_at - first) as f64);
        }
    }

    /// A reply finished: its real count (when the provider gave one) replaces the estimate.
    pub fn reply_done(&mut self, output_tokens: u64) {
        let ms = self.first_at.map(|f| (self.now)() - f).unwrap_or(0);
        // The real count over the same time, unless it holds tokens never streamed (reasoning a
        // provider keeps to itself, made before the first delta): then the estimate.
        let out = output_tokens as f64;
        let counted = if output_tokens > 0 && out <= self.streaming * 2.0 + 20.0 {
            out
        } else {
            self.streaming
        };
        if ms >= RATE_MIN_MS {
            self.rate = Some(counted * 1000.0 / ms as f64);
        }
        self.settled += if output_tokens > 0 {
            out
        } else {
            js_round(self.streaming)
        };
        self.streaming = 0.0;
        self.first_at = None;
    }

    pub fn tokens(&self) -> u64 {
        self.shown
    }

    pub fn tick(&mut self) {
        self.frame = (self.frame + 1) % FRAMES.len();
        let target = js_round(self.settled + self.streaming).max(0.0) as u64;
        // A third of the way each tick: fast while far behind, slowing near the end (about a
        // second to catch up); an estimate that was too high snaps down.
        self.shown = if self.shown < target {
            self.shown + 1.max((target - self.shown).div_ceil(3))
        } else {
            target
        };
        self.update();
    }

    fn update(&mut self) {
        let mut parts = vec![fmt_elapsed((self.now)() - self.started)];
        if self.shown > 0 {
            parts.push(t("↓ {n} トークン", &[("n", &fmt_count(self.shown))]));
        }
        if let Some(rate) = self.rate {
            parts.push(format!("{} tok/s", fmt_rate(rate)));
        }
        parts.push(t("esc で中断", &[]));
        let what = self.detail.clone().unwrap_or_else(|| self.phrase.clone());
        self.text.set_text(format!(
            "{} {} {}",
            (self.spinner_color)(FRAMES[self.frame]),
            (self.text_color)(&what),
            (self.text_color)(&format!("({})", parts.join(" · ")))
        ));
    }
}

impl Component for Activity {
    fn render(&mut self, width: usize) -> Vec<String> {
        let mut out = vec![String::new()];
        out.extend(self.text.render(width));
        out
    }

    fn invalidate(&mut self) {
        self.text.invalidate();
    }
}

#[cfg(test)]
mod tests {
    //! activity.test.ts
    use std::sync::Arc;
    use std::sync::atomic::{AtomicI64, Ordering};

    use super::*;

    fn id(s: &str) -> String {
        s.to_string()
    }

    /// The activity line at a clock the test moves, re-rendered.
    fn line(clock: &Arc<AtomicI64>) -> Activity {
        let c = clock.clone();
        Activity::new(id, id, Box::new(|| 0.5), Box::new(move || c.load(Ordering::Relaxed)))
    }

    fn text(a: &mut Activity) -> String {
        a.set_detail(None);
        a.render(200).join("\n")
    }

    // "fmtRate: one decimal below 10 a second, whole numbers above"
    #[test]
    fn fmt_rate_one_decimal_below_ten() {
        assert_eq!(fmt_rate(3.24), "3.2");
        assert_eq!(fmt_rate(42.6), "43");
    }

    // "tok/s: measured from the first delta, shown once half a second has streamed"
    #[test]
    fn rate_measured_from_the_first_delta() {
        let _g = crate::testing::global_lock();
        let clock = Arc::new(AtomicI64::new(0));
        let mut a = line(&clock);
        a.new_reply();
        clock.store(3000, Ordering::Relaxed);
        a.streamed(&"x".repeat(400));
        clock.store(3200, Ordering::Relaxed);
        a.streamed(&"x".repeat(400));
        assert!(!text(&mut a).contains("tok/s"));
        clock.store(4000, Ordering::Relaxed);
        a.streamed(&"x".repeat(400));
        assert!(text(&mut a).contains("300 tok/s"));
    }

    // "tok/s: the provider's count replaces the estimate at the end, and stays while tools run"
    #[test]
    fn provider_count_replaces_the_estimate() {
        let _g = crate::testing::global_lock();
        let clock = Arc::new(AtomicI64::new(0));
        let mut a = line(&clock);
        a.new_reply();
        a.streamed(&"x".repeat(400));
        clock.store(2000, Ordering::Relaxed);
        a.reply_done(120);
        assert!(text(&mut a).contains("60 tok/s"));
        a.set_detail(Some("read を実行中…".into()));
        clock.store(9000, Ordering::Relaxed);
        assert!(a.render(200).join("\n").contains("60 tok/s"));
    }

    // "tok/s: a count far above what streamed (hidden reasoning) falls back to the estimate"
    #[test]
    fn hidden_reasoning_falls_back_to_the_estimate() {
        let _g = crate::testing::global_lock();
        let clock = Arc::new(AtomicI64::new(0));
        let mut a = line(&clock);
        a.new_reply();
        a.streamed(&"x".repeat(400));
        clock.store(1000, Ordering::Relaxed);
        a.reply_done(5000);
        assert!(text(&mut a).contains("100 tok/s"));
    }
}
