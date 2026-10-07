import type { Agent, ApprovalAnswer, ApprovalRequest } from "@sasacode/agent";
import { t } from "@sasacode/host";

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
  /** Stop reading. Call once the run is over. */
  stop(): void;
}

/**
 * Lets the process that started this run answer for the user. It reads JSON Lines from `input`:
 *   {"type":"approval","id":…,"decision":"allow"|"always"|"deny","feedback"?:…}  answers a request
 *   {"type":"abort"}                                                             stops the run
 * and asks for approval by writing `approval_request` through `emit` (one JSON object per line,
 * written by the caller's own writer so lines never interleave with the run's events).
 * Anything it cannot use becomes a `control_error` line. When `input` ends, every pending and
 * later request is denied; the run goes on.
 */
export function startControl(agent: Pick<Agent, "abort" | "approve">, input: ControlInput, emit: (message: object) => void): Control {
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
      try {
        emit({ type: "approval_request", id, callId: req.call.id, tool: req.tool.name, args: req.args, reason: req.reason, cwd: req.cwd });
      } catch (e) {
        pending.delete(id);
        req.signal?.removeEventListener("abort", onAbort);
        resolve({ decision: "deny", feedback: e instanceof Error ? e.message : String(e) });
      }
    });

  const fail = (error: string) => emit({ type: "control_error", error });

  const handle = (line: string) => {
    let msg: any;
    try {
      msg = JSON.parse(line);
    } catch {
      return fail("not valid JSON");
    }
    if (!msg || typeof msg !== "object" || Array.isArray(msg)) return fail("a control message is a JSON object");
    if (msg.type === "abort") return agent.abort();
    if (msg.type !== "approval") return fail(`unknown message type ${JSON.stringify(msg.type) ?? "(none)"}`);
    if (typeof msg.id !== "string") return fail('approval needs a string "id"');
    if (msg.decision !== "allow" && msg.decision !== "always" && msg.decision !== "deny") return fail('approval "decision" must be allow, always or deny');
    if (msg.feedback !== undefined && typeof msg.feedback !== "string") return fail('approval "feedback" must be a string');
    const answer = pending.get(msg.id);
    if (!answer) return fail(`no pending approval with id ${JSON.stringify(msg.id)} (unknown, already answered or cancelled)`);
    pending.delete(msg.id);
    answer({ decision: msg.decision, ...(msg.feedback !== undefined && { feedback: msg.feedback }) });
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
  })();

  return {
    stop() {
      stopped = true;
      closed = true; // nobody reads any more: a later request is denied, not left waiting
      void Promise.resolve(iterator.return?.()).catch(() => {});
    },
  };
}
