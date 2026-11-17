//! `plugins/jev-guard/lang.ts`: the language the user reads (the host's `ui.lang`), set when the
//! plugin loads: notices, reasons and labels come in Japanese or English.

use std::sync::atomic::{AtomicBool, Ordering};

use sasacode_plugin_api::Lang;

static ENGLISH: AtomicBool = AtomicBool::new(false);

pub fn set_lang(lang: Lang) {
    ENGLISH.store(lang == Lang::En, Ordering::Relaxed);
}

/// The Japanese or the English text, whichever the user reads.
#[allow(non_snake_case)]
pub fn L(ja: impl Into<String>, en: impl Into<String>) -> String {
    if ENGLISH.load(Ordering::Relaxed) {
        en.into()
    } else {
        ja.into()
    }
}
