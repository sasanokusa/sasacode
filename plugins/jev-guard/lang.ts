// The language the user reads (the host's ui.lang), set when the plugin loads: notices, reasons
// and labels come in Japanese or English.
let english = false;

export function setLang(lang: string | undefined): void {
  english = lang === "en";
}

/** The Japanese or the English text, whichever the user reads. */
export const L = (ja: string, en: string): string => (english ? en : ja);
