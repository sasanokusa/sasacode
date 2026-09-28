import { text, type Plugin, type PluginUI, type SelectOption, type ToolContext, type ToolResult } from "@sasacode/plugin-api";

interface Option { label: string; description?: string }
interface Question { question: string; options: Option[]; multiSelect?: boolean }
interface Args { questions: Question[] }
export interface Answer { question: string; answer?: string[]; own?: string; outcome: "chosen" | "own_words" | "dismissed" }

const OTHER = "\0other";
const labels = {
  ja: {
    other: "その他（自分で答える）", otherHint: "選択肢にない答えを入力する",
    otherChat: "その他（チャットで答える）", otherChatHint: "選択肢にない答えをチャットで伝える",
    count: (i: number, n: number) => `質問 ${i}/${n}`, own: "あなたの答え（空のまま enter で取り消し）",
  },
  en: {
    other: "Other (type an answer)", otherHint: "Answer with something not listed",
    otherChat: "Other (answer in chat)", otherChatHint: "Type an answer that is not listed in the chat",
    count: (i: number, n: number) => `Question ${i}/${n}`, own: "Your answer (enter on empty to cancel)",
  },
};

/** Resolves undefined when the run is interrupted; the dialog itself stays with the host. */
function untilAborted<T>(work: Promise<T>, signal: AbortSignal): Promise<T | undefined> {
  if (signal.aborted) return Promise.resolve(undefined);
  return new Promise((resolve, reject) => {
    const onAbort = () => resolve(undefined);
    signal.addEventListener("abort", onAbort, { once: true });
    work.then(resolve, reject).finally(() => signal.removeEventListener("abort", onAbort));
  });
}

/** Hosts before API 1.10 have neither free text nor multiple choice. */
const has = <K extends "input" | "selectMany">(ui: PluginUI, k: K) => typeof ui[k] === "function";

export async function ask(ui: PluginUI, args: Args, ctx: ToolContext): Promise<ToolResult> {
  if (!ui.interactive)
    return { content: [text("No user is available to answer (non-interactive run). Decide yourself, and state the assumption you made in your reply.")], details: { answers: [] } };
  const l = labels[ui.lang] ?? labels.en;
  const typed = has(ui, "input");
  const answers: Answer[] = [];
  const interrupted = () => ({ content: [text("Interrupted before the user answered.")], isError: true, details: { answers } });
  for (const [i, q] of args.questions.entries()) {
    const title = args.questions.length > 1 ? `${l.count(i + 1, args.questions.length)}\n${q.question}` : q.question;
    const options: SelectOption[] = q.options.map((o, n) => ({ value: String(n), label: o.label, description: o.description }));
    options.push(typed ? { value: OTHER, label: l.other, description: l.otherHint } : { value: OTHER, label: l.otherChat, description: l.otherChatHint });
    const many = q.multiSelect && has(ui, "selectMany");
    const picked = many
      ? await untilAborted(ui.selectMany(title, options), ctx.signal)
      : await untilAborted(ui.select(title, options), ctx.signal).then((v) => v === undefined ? undefined : [v]);
    if (ctx.signal.aborted) return interrupted();
    if (picked === undefined || (many && picked.length === 0)) { answers.push({ question: q.question, outcome: "dismissed" }); break; }
    const chosen = picked.filter((v) => v !== OTHER).map((v) => q.options[Number(v)]!.label);
    if (!picked.includes(OTHER)) { answers.push({ question: q.question, answer: chosen, outcome: "chosen" }); continue; }
    if (!typed) { answers.push({ question: q.question, answer: chosen, outcome: "own_words" }); break; }
    const own = (await untilAborted(ui.input(title, { message: l.own }), ctx.signal))?.trim();
    if (ctx.signal.aborted) return interrupted();
    if (!own) { answers.push({ question: q.question, outcome: "dismissed" }); break; }
    answers.push({ question: q.question, answer: chosen, own, outcome: "chosen" });
  }
  return { content: [text(report(answers, args.questions.length))], details: { answers } };
}

export function report(answers: Answer[], asked: number): string {
  const lines = answers.filter((a) => a.outcome === "chosen").map((a) => {
    const parts = [...(a.answer ?? []), ...(a.own ? [`(in the user's own words) ${a.own}`] : [])];
    return `Q: ${a.question}\nA: ${parts.join("; ")}`;
  });
  const last = answers.at(-1);
  if (last?.outcome === "own_words")
    lines.push(`Q: ${last.question}\n${last.answer?.length ? `Chosen so far: ${last.answer.join("; ")}. ` : ""}The user wants to answer in their own words. End your turn now with a short message and wait for their reply; do not guess the answer.`);
  else if (last?.outcome === "dismissed")
    lines.push(`Q: ${last.question}\nThe user dismissed the question. Do not assume an answer: say what you need and wait, or continue only with work that does not depend on it.`);
  if (answers.length < asked) lines.push(`${asked - answers.length} later question(s) were not asked.`);
  return lines.join("\n\n");
}

const plugin: Plugin = (api) => {
  api.registerTool({
    name: "ask_user",
    description: "Ask the user one to four multiple-choice questions and wait for the answers. Use it when a decision is genuinely the user's (requirements, preferences, trade-offs) and cannot be settled from the request, the code or a sensible default; not for confirming routine steps or asking permission. Give 2-6 distinct options per question, recommended one first; set multiSelect when several can apply. The user can always answer in their own words instead.",
    kind: "other",
    parameters: {
      type: "object",
      properties: {
        questions: {
          type: "array", minItems: 1, maxItems: 4,
          items: {
            type: "object",
            properties: {
              question: { type: "string", minLength: 1, maxLength: 500, description: "The full question, ending with a question mark." },
              multiSelect: { type: "boolean", description: "Let the user pick several options (not mutually exclusive choices)." },
              options: {
                type: "array", minItems: 2, maxItems: 6,
                items: {
                  type: "object",
                  properties: {
                    label: { type: "string", minLength: 1, maxLength: 80, description: "Short answer, 1-5 words." },
                    description: { type: "string", maxLength: 200, description: "What choosing this means." },
                  },
                  required: ["label"], additionalProperties: false,
                },
              },
            },
            required: ["question", "options"], additionalProperties: false,
          },
        },
      },
      required: ["questions"], additionalProperties: false,
    },
    summary: (args: Args) => args.questions?.[0]?.question ?? "",
    execute: (args: Args, ctx) => ask(api.ui, args, ctx),
  });
  // Asking whether it may ask would defeat the tool. A user's own deny or ask rule still wins.
  api.permissions.addRules({ allow: ["ask_user"] });
};
export default plugin;
