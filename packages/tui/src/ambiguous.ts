import { setAmbiguousWidth } from "@earendil-works/pi-tui";

/**
 * East Asian Ambiguous characters (①, ○, ※ in some fonts, …) take one cell in most terminals
 * but two in ones set up for CJK. If the TUI counts them differently from the terminal, lines
 * overlap. Ask the terminal: print one, then read the cursor position back (DSR). The answer
 * can be forced with SASACODE_AMBIGUOUS_WIDTH=1 or 2.
 */
export async function matchAmbiguousWidth(timeoutMs = 400): Promise<void> {
  const forced = process.env.SASACODE_AMBIGUOUS_WIDTH;
  if (forced === "1" || forced === "2") return setAmbiguousWidth(Number(forced) as 1 | 2);
  const width = await measure(timeoutMs);
  if (width) setAmbiguousWidth(width);
}

async function measure(timeoutMs: number): Promise<1 | 2 | undefined> {
  const { stdin, stdout } = process;
  if (!stdin.isTTY || !stdout.isTTY) return undefined;
  const wasRaw = stdin.isRaw;
  stdin.setRawMode(true);
  stdin.resume();
  try {
    return await new Promise((resolve) => {
      let buf = "";
      const done = (w: 1 | 2 | undefined) => {
        clearTimeout(timer);
        stdin.off("data", onData);
        resolve(w);
      };
      const onData = (d: Buffer) => {
        buf += d.toString("utf8");
        const m = /\x1b\[\d+;(\d+)R/.exec(buf);
        // From column 1, one character of width w leaves the cursor at column 1 + w.
        if (m) done(Number(m[1]) === 3 ? 2 : 1);
      };
      const timer = setTimeout(() => done(undefined), timeoutMs);
      stdin.on("data", onData);
      stdout.write("\r①\x1b[6n");
    });
  } finally {
    stdout.write("\r\x1b[2K");
    stdin.setRawMode(wasRaw);
    stdin.pause();
  }
}
