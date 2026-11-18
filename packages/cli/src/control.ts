import { type Agent, type ApprovalAnswer, type ApprovalRequest, headlessUI, PermissionPolicy, type PluginHost, type UIBridge } from "@sasacode/agent";
import { onJudged } from "@sasacode/bundled";
import { t } from "@sasacode/host";
import type { SelectOption } from "@sasacode/plugin-api";

/** What `--control stdio` reads: the parent's stdin, as chunks of bytes or text. */
export type ControlInput = AsyncIterable<Uint8Array | string>;

const CLOSED = "the controlling process closed stdin";

/**
 * `--control`: only `stdio` exists, and only for a run a parent can read as JSON Lines
 * (`-p` with `--output jsonl`). Returns whether the control channel is on; anything else throws.
 */
export function controlMode(control: string | undefined, print: string | undefined, output: string | undefined): boolean {
  if (control === undefined) return false;
  if (control !== "stdio") throw new Error(t("--control must be stdio"));
  if (print === undefined || output !== "jsonl") throw new Error(t("--control stdio needs -p and --output jsonl"));
  return true;
}

export interface Control {
  /** api.ui for the plugins: their questions go to the parent (ui_request), their notices and texts are lines too. */
  ui: UIBridge;
  /** Report the plugins' status items (plugin_status) and slash commands (commands) as they are now, then as they change. */
  watch(host: Pick<PluginHost, "status" | "commands" | "onChange">): void;
  /** Abort the run, and dismiss the plugins' questions still open (what SIGINT does). */
  abort(): void;
  /** Stop reading. Call once the run is over. */
  stop(): void;
}

/** The allow rules an "always" answer adds for this call: each one can be given back with --allow. */
function alwaysRules(req: ApprovalRequest): string[] {
  try {
    return PermissionPolicy.rulesFor(req);
  } catch {
    return []; // a tool whose matchTarget or paths throws: the call is asked about all the same
  }
}

/** An option as the parent sees it: the plugin's object may carry more. */
const shown = (o: SelectOption) => ({ value: o.value, label: o.label, ...(o.description !== undefined && { description: o.description }) });

/**
 * Lets the process that started this run answer for the user. It reads JSON Lines from `input`:
 *   {"type":"approval","id":…,"decision":"allow"|"always"|"deny","feedback"?:…,"args"?:{…}}  answers a request
 *   {"type":"abort","callId"?:…}                                stops the run, or with callId that tool call alone
 *   {"type":"message","text":…}                                 tells the model something mid-run
 *   {"type":"ui_response","id":…,"value":…}                     answers a plugin's question (null dismisses it)
 * and asks for approval by writing `approval_request` through `emit` (one JSON object per line,
 * written by the caller's own writer so lines never interleave with the run's events). The plugins
 * reach the parent through `ui` (ui_request, plugin_notify, plugin_text) and `watch` (plugin_status,
 * commands); the agent permission mode's judge is reported as `judged`.
 * Anything it cannot use becomes a `control_error` line. When `input` ends, every pending and
 * later request is denied, and every question dismissed; the run goes on.
 */
export function startControl(
  agent: Pick<Agent, "abort" | "approve" | "inject"> & Partial<Pick<Agent, "abortCall">>,
  input: ControlInput,
  emit: (message: object) => void,
): Control {
  const pending = new Map<string, (answer: ApprovalAnswer) => void>();
  let seq = 0;
  let closed = false;
  let stopped = false;

  agent.approve = (req: ApprovalRequest) =>
    new Promise<ApprovalAnswer>((resolve) => {
      const closedAnswer: ApprovalAnswer = { decision: "deny", feedback: CLOSED };
      if (closed) return resolve(closedAnswer);
      // Nobody was asked, so nobody is told it was cancelled.
      if (req.signal?.aborted) return resolve({ decision: "deny" });
      const id = `approval-${++seq}`;
      const onAbort = () => {
        if (!pending.delete(id)) return;
        emit({ type: "approval_cancelled", id });
        resolve({ decision: "deny" }); // like the TUI: the run is being interrupted, the call does not run
      };
      pending.set(id, (answer) => {
        req.signal?.removeEventListener("abort", onAbort);
        resolve(answer);
      });
      req.signal?.addEventListener("abort", onAbort, { once: true });
      const rules = alwaysRules(req);
      try {
        emit({ type: "approval_request", id, callId: req.call.id, tool: req.tool.name, args: req.args, reason: req.reason, cwd: req.cwd, rules });
      } catch (e) {
        pending.delete(id);
        req.signal?.removeEventListener("abort", onAbort);
        resolve({ decision: "deny", feedback: e instanceof Error ? e.message : String(e) });
      }
    });

  // ── the plugins' questions (api.ui) ──
  interface Question {
    /** Why `value` cannot answer it, or undefined when it can. */
    check(value: unknown): string | undefined;
    /** null: dismissed, answered as a headless run answers. */
    settle(value: unknown): void;
  }
  const questions = new Map<string, Question>();
  let asked = 0;
  const question = <T>(kind: string, fields: object, dismissed: T, fits: (v: unknown) => boolean, expected: string) =>
    new Promise<T>((resolve) => {
      if (closed) return resolve(dismissed);
      const id = `ui-${++asked}`;
      questions.set(id, {
        check: (v) => (v === null || fits(v) ? undefined : `ui_response to a ${kind} needs "value": ${expected}, or null`),
        settle: (v) => resolve(v === null ? dismissed : (v as T)),
      });
      try {
        emit({ type: "ui_request", id, kind, ...fields });
      } catch {
        questions.delete(id);
        resolve(dismissed);
      }
    });
  /** Every open question answered as dismissed; `announce`: the parent is told (ui_cancelled). */
  const dismiss = (announce: boolean) => {
    const open = [...questions];
    questions.clear();
    for (const [id, q] of open) {
      if (announce) emit({ type: "ui_cancelled", id });
      q.settle(null);
    }
  };
  const offered = (options: SelectOption[], v: unknown) => typeof v === "string" && options.some((o) => o.value === v);
  const ui: UIBridge = {
    // A parent that has closed stdin answers nothing more: plugins see a headless run from then on.
    get interactive() {
      return !closed;
    },
    notify: (text, level) => (stopped ? headlessUI.notify(text, level) : emit({ type: "plugin_notify", level, text })),
    async showText({ title, text, format }) {
      if (stopped) headlessUI.notify(`${title}\n${text}`, "info");
      else emit({ type: "plugin_text", title, text, format: format ?? "text" });
    },
    confirm: (title, message) => question("confirm", { title, message }, false, (v) => typeof v === "boolean", "true or false"),
    select: (title, options) =>
      question<string | undefined>("select", { title, options: options.map(shown) }, undefined, (v) => offered(options, v), "one of the options' values"),
    input: (title, o) =>
      // Named one by one: a plugin's options object carrying `id` or `type` must not change the line's own.
      question<string | undefined>("input", { title, message: o.message, placeholder: o.placeholder, initial: o.initial }, undefined, (v) => typeof v === "string", "a string"),
    selectMany: (title, options, o) =>
      question<string[] | undefined>(
        "select_many",
        { title, options: options.map(shown), selected: o.selected },
        undefined,
        (v) => Array.isArray(v) && v.every((x) => offered(options, x)),
        "an array of the options' values",
      ),
  };

  const abortRun = () => {
    dismiss(true); // a tool waiting on a question would otherwise keep the run from ending
    agent.abort();
  };
  const unjudged = onJudged((judged) => emit({ type: "judged", ...judged }));

  const fail = (error: string) => emit({ type: "control_error", error });

  const handle = (line: string) => {
    let msg: any;
    try {
      msg = JSON.parse(line);
    } catch {
      return fail("not valid JSON");
    }
    if (!msg || typeof msg !== "object" || Array.isArray(msg)) return fail("a control message is a JSON object");
    if (msg.type === "abort") {
      if (msg.callId === undefined) return abortRun();
      if (typeof msg.callId !== "string") return fail('abort "callId" must be a string');
      // That call alone: its result is an error, and the run goes on.
      if (!agent.abortCall?.(msg.callId)) return fail(`no running tool call with callId ${JSON.stringify(msg.callId)}`);
      return;
    }
    if (msg.type === "message") {
      if (typeof msg.text !== "string" || !msg.text.trim()) return fail('message needs a non-empty string "text"');
      // Like a message typed while the TUI is busy: the model sees it at its next turn.
      return agent.inject([{ type: "text", text: msg.text }], "next");
    }
    if (msg.type === "ui_response") {
      if (typeof msg.id !== "string") return fail('ui_response needs a string "id"');
      const q = questions.get(msg.id);
      if (!q) return fail(`no pending ui_request with id ${JSON.stringify(msg.id)} (unknown, already answered or cancelled)`);
      if (!("value" in msg)) return fail('ui_response needs "value" (null dismisses the question)');
      const problem = q.check(msg.value);
      if (problem) return fail(problem);
      questions.delete(msg.id);
      return q.settle(msg.value);
    }
    if (msg.type !== "approval") return fail(`unknown message type ${JSON.stringify(msg.type) ?? "(none)"}`);
    if (typeof msg.id !== "string") return fail('approval needs a string "id"');
    if (msg.decision !== "allow" && msg.decision !== "always" && msg.decision !== "deny") return fail('approval "decision" must be allow, always or deny');
    if (msg.feedback !== undefined && typeof msg.feedback !== "string") return fail('approval "feedback" must be a string');
    const edited = msg.args !== undefined;
    if (edited && (!msg.args || typeof msg.args !== "object" || Array.isArray(msg.args) || msg.decision === "deny"))
      return fail('approval "args" must be an object, sent with allow or always');
    const answer = pending.get(msg.id);
    if (!answer) return fail(`no pending approval with id ${JSON.stringify(msg.id)} (unknown, already answered or cancelled)`);
    pending.delete(msg.id);
    answer({ decision: msg.decision, ...(msg.feedback !== undefined && { feedback: msg.feedback }), ...(edited && { args: msg.args }) });
  };

  const iterator = input[Symbol.asyncIterator]();
  void (async () => {
    const decoder = new TextDecoder();
    let buffered = "";
    const feed = (text: string) => {
      buffered += text;
      for (let nl = buffered.indexOf("\n"); nl >= 0; nl = buffered.indexOf("\n")) {
        const line = buffered.slice(0, nl).trim();
        buffered = buffered.slice(nl + 1);
        if (line) handle(line);
      }
    };
    try {
      for (let r = await iterator.next(); !r.done && !stopped; r = await iterator.next())
        feed(typeof r.value === "string" ? r.value : decoder.decode(r.value, { stream: true }));
    } catch {
      // A broken pipe ends the channel like EOF does.
    }
    if (stopped) return;
    feed(`${decoder.decode()}\n`); // a last line without its newline still counts
    closed = true;
    for (const answer of [...pending.values()]) answer({ decision: "deny", feedback: CLOSED });
    pending.clear();
    dismiss(false); // nobody is there to be told
  })();

  return {
    ui,
    watch(host) {
      const statuses = new Map<string, string>();
      let listed = "";
      const report = () => {
        if (stopped) return;
        for (const [key, text] of host.status)
          if (statuses.get(key) !== text) {
            statuses.set(key, text);
            emit({ type: "plugin_status", key, text });
          }
        for (const key of [...statuses.keys()])
          if (!host.status.has(key)) {
            statuses.delete(key);
            emit({ type: "plugin_status", key, text: null });
          }
        const commands = host.commands.map((c) => ({ name: c.name, description: c.description, argumentHint: c.argumentHint, owner: c.owner }));
        if (JSON.stringify(commands) === listed) return;
        listed = JSON.stringify(commands);
        emit({ type: "commands", commands });
      };
      const before = host.onChange;
      host.onChange = () => {
        before?.();
        report();
      };
      report();
    },
    abort: abortRun,
    stop() {
      stopped = true;
      closed = true; // nobody reads any more: a later request is denied, not left waiting
      dismiss(true);
      unjudged();
      void Promise.resolve(iterator.return?.()).catch(() => {});
    },
  };
}
