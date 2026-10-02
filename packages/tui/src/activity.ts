// The line under the conversation while a run is going: what it is doing, for how long, and how
// many tokens the model has produced so far (counted up as they stream in).
import { Text, type TUI } from "@earendil-works/pi-tui";
import { currentLang, t } from "@sasacode/host";

const FRAMES = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const TICK_MS = 80;

/** Shown while the model works: one per reply, never the same one twice in a row. */
export const PHRASES: Record<"ja" | "en", string[]> = {
  ja: [
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
  ],
  en: [
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
  ],
};

/** Tokens in streamed text, roughly: about 4 characters each in English, fewer in Japanese. */
export function estimateTokens(s: string): number {
  let ascii = 0;
  for (let i = 0; i < s.length; i++) if (s.charCodeAt(i) < 128) ascii++;
  return ascii / 4 + (s.length - ascii) / 1.3;
}

export function fmtCount(n: number): string {
  if (n < 1000) return String(n);
  return n < 10_000 ? `${(n / 1000).toFixed(1)}k` : `${Math.round(n / 1000)}k`;
}

function fmtElapsed(ms: number): string {
  const s = Math.floor(ms / 1000);
  return s < 60 ? t("{s}秒", { s }) : t("{m}分{s}秒", { m: Math.floor(s / 60), s: s % 60 });
}

export class Activity extends Text {
  private frame = 0;
  private timer?: ReturnType<typeof setInterval>;
  private readonly started = Date.now();
  private phrase = "";
  private replies = 0;
  private detail?: string;
  /** Output tokens of the replies finished in this run, as the provider reported them. */
  private settled = 0;
  /** The reply streaming now, estimated from its text. */
  private streaming = 0;
  /** What the line shows: it climbs toward settled + streaming a step each tick. */
  private shown = 0;

  constructor(
    private readonly ui: TUI,
    private readonly spinnerColor: (s: string) => string,
    private readonly textColor: (s: string) => string,
    private readonly random: () => number = Math.random,
  ) {
    super("", 1, 0);
    this.nextPhrase();
    this.tick();
    this.timer = setInterval(() => this.tick(), TICK_MS);
  }

  render(width: number): string[] {
    return ["", ...super.render(width)];
  }

  /** A reply starts streaming: back from a tool or a check, with a new phrase after the first. */
  newReply(): void {
    if (this.replies++) this.nextPhrase();
    this.detail = undefined;
  }

  /** A reply thrown away (cut off, to be asked for again): its tokens no longer count. */
  discardReply(): void {
    this.streaming = 0;
  }

  private nextPhrase(): void {
    const list = PHRASES[currentLang()];
    let next = this.phrase;
    for (let i = 0; i < 5 && next === this.phrase; i++) next = list[Math.floor(this.random() * list.length)]!;
    this.phrase = next;
  }

  /** What the run waits on besides the model (a permission check, a tool); undefined for the model. */
  setDetail(text: string | undefined): void {
    this.detail = text;
    this.update();
  }

  streamed(delta: string): void {
    this.streaming += estimateTokens(delta);
  }

  /** A reply finished: its real count (when the provider gave one) replaces the estimate. */
  replyDone(outputTokens: number): void {
    this.settled += outputTokens > 0 ? outputTokens : Math.round(this.streaming);
    this.streaming = 0;
  }

  get tokens(): number {
    return this.shown;
  }

  stop(): void {
    clearInterval(this.timer);
    this.timer = undefined;
  }

  private tick(): void {
    this.frame = (this.frame + 1) % FRAMES.length;
    const target = Math.round(this.settled + this.streaming);
    // A third of the way each tick: fast while far behind, slowing near the end (about a second
    // to catch up); an estimate that was too high snaps down.
    this.shown = this.shown < target ? this.shown + Math.max(1, Math.ceil((target - this.shown) / 3)) : target;
    this.update();
  }

  private update(): void {
    const parts = [fmtElapsed(Date.now() - this.started)];
    if (this.shown > 0) parts.push(t("↓ {n} トークン", { n: fmtCount(this.shown) }));
    parts.push(t("esc で中断"));
    this.setText(`${this.spinnerColor(FRAMES[this.frame]!)} ${this.textColor(this.detail ?? this.phrase)} ${this.textColor(`(${parts.join(" · ")})`)}`);
    this.ui.requestRender();
  }
}
