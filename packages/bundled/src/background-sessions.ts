// background-sessions — run long CLI tasks in the background and get re-fired when they finish.
//
// Instead of the model polling a long build/test/train command, `bg_start` returns immediately
// and, when the command exits, a notification is injected into the session (`session.inject "now"`,
// which starts a run if the agent is idle) with the exit code and the tail of the output.
//
// Jobs are detached processes with their own process group, so they outlive sasacode itself; the
// plugin adopts still-running jobs on the next start and notifies about the ones that finished
// while it was away. State lives in `~/.sasacode/background-sessions/<id>/` (meta.json, output.log,
// exit-code; under SASACODE_HOME when that is set).
//
// Security: bg_start declares `permissionsAs: "bash"` (plugin API 1.6), so it is judged by the
// rules written for bash — the user's `bash(sudo *)` deny/ask rules, the guard preset, `bash(...)`
// allow rules — matched against its command. It cannot become a way around them, and rules under
// its own name (`bg_start(make*)`) still apply. Requires plugin API 1.6 (sasacode 0.9+).
//
// Settings (`plugins.settings.background-sessions`): dir (storage path), pollMs (liveness check),
// tailLines (lines of output in the completion notice), keep (finished jobs retained),
// autoNotify (default true — set false to only show a UI notification without re-firing the agent),
// foreignGraceMs (how long the session that started a job gets to claim its completion notice
// before another session reports it).
//
// POSIX only (bash, kill(-pid)). Tool descriptions are English (model-facing); notices are
// Japanese (user-facing).

import { errorResult, text, type Plugin, type ToolDefinition } from "@sasacode/plugin-api";
import { spawn } from "node:child_process";
import {
  closeSync,
  mkdirSync,
  openSync,
  readFileSync,
  readSync,
  readdirSync,
  rmSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";

type Status = "running" | "done" | "killed";

interface Meta {
  id: string;
  command: string;
  cwd: string;
  runCwd: string;
  pid: number;
  startedAt: number;
  status: Status;
  /** Session that started the job; its completion notice goes there first. */
  sessionId?: string;
  exitCode?: number | null;
  finishedAt?: number;
  notified?: boolean;
  killed?: boolean;
  spawnError?: string;
}

interface Job extends Meta {
  dir: string;
  logPath: string;
  exitPath: string;
  child?: ReturnType<typeof spawn>;
}

interface Settings {
  dir?: string;
  pollMs?: number;
  tailLines?: number;
  keep?: number;
  autoNotify?: boolean;
  /** ms the session that started a job gets to claim its completion notice (default 10000). */
  foreignGraceMs?: number;
}

const DEFAULT_POLL_MS = 1500;
const DEFAULT_TAIL_LINES = 30;
const DEFAULT_KEEP = 40;
const DEFAULT_FOREIGN_GRACE_MS = 10_000;
const MAX_LOG_BYTES = 32 * 1024 * 1024; // larger logs: read them with bash tail/sed instead
const TAIL_READ_BYTES = 256 * 1024;
const DEFAULT_PAGE_LINES = 200;
const MAX_PAGE_LINES = 1000;

const num = (v: unknown, d: number, min = 1): number =>
  typeof v === "number" && isFinite(v) && v >= min ? v : d;

function fmtDur(ms: number): string {
  const s = Math.max(0, Math.round(ms / 1000));
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m${String(s % 60).padStart(2, "0")}s`;
  return `${Math.floor(m / 60)}h${String(m % 60).padStart(2, "0")}m`;
}

function isAlive(pid: number): boolean {
  if (!pid) return false;
  try {
    process.kill(pid, 0);
    return true;
  } catch (e) {
    return (e as NodeJS.ErrnoException).code === "EPERM";
  }
}

function mtimeOf(path: string): number | undefined {
  try {
    return statSync(path).mtimeMs;
  } catch {
    return undefined;
  }
}

const plugin: Plugin = (api) => {
  const s = api.settings as Settings;
  const home = process.env.SASACODE_HOME ?? join(homedir(), ".sasacode");
  const root = typeof s.dir === "string" && s.dir ? s.dir : join(home, "background-sessions");
  const pollMs = num(s.pollMs, DEFAULT_POLL_MS, 50);
  const tailLines = num(s.tailLines, DEFAULT_TAIL_LINES);
  const keep = num(s.keep, DEFAULT_KEEP);
  const foreignGraceMs = num(s.foreignGraceMs, DEFAULT_FOREIGN_GRACE_MS, 0);
  const autoNotify = s.autoNotify !== false;
  const jobs = new Map<string, Job>();

  try {
    mkdirSync(root, { recursive: true });
  } catch (e) {
    api.log(`background-sessions: cannot create ${root}: ${e}`);
  }

  // ── storage ────────────────────────────────────────────────────────

  const metaPath = (dir: string) => join(dir, "meta.json");
  const logPathOf = (dir: string) => join(dir, "output.log");
  const exitPathOf = (dir: string) => join(dir, "exit-code");

  function readMeta(dir: string): Meta | undefined {
    try {
      return JSON.parse(readFileSync(metaPath(dir), "utf8")) as Meta;
    } catch {
      return undefined;
    }
  }

  function diskMetas(): { meta: Meta; dir: string }[] {
    const out: { meta: Meta; dir: string }[] = [];
    let names: string[] = [];
    try {
      names = readdirSync(root);
    } catch {
      return out;
    }
    for (const n of names) {
      const dir = join(root, n);
      const meta = readMeta(dir);
      if (meta?.id) out.push({ meta, dir });
    }
    return out;
  }

  function save(job: Job): void {
    const meta: Meta = {
      id: job.id,
      command: job.command,
      cwd: job.cwd,
      runCwd: job.runCwd,
      pid: job.pid,
      startedAt: job.startedAt,
      status: job.status,
    };
    if (job.sessionId !== undefined) meta.sessionId = job.sessionId;
    if (job.exitCode !== undefined) meta.exitCode = job.exitCode;
    if (job.finishedAt !== undefined) meta.finishedAt = job.finishedAt;
    if (job.notified !== undefined) meta.notified = job.notified;
    if (job.killed !== undefined) meta.killed = job.killed;
    if (job.spawnError !== undefined) meta.spawnError = job.spawnError;
    try {
      writeFileSync(metaPath(job.dir), JSON.stringify(meta, null, 2));
    } catch (e) {
      api.log(`background-sessions: cannot write meta for ${job.id}: ${e}`);
    }
  }

  function readExitCode(exitPath: string): number | null {
    try {
      const v = parseInt(readFileSync(exitPath, "utf8").trim(), 10);
      return isFinite(v) ? v : null;
    } catch {
      return null;
    }
  }

  /** Last `lines` lines of a log, reading at most TAIL_READ_BYTES from the end. */
  function tailOf(path: string, lines: number): string {
    try {
      const size = statSync(path).size;
      const start = Math.max(0, size - TAIL_READ_BYTES);
      const fd = openSync(path, "r");
      const buf = Buffer.alloc(size - start);
      try {
        readSync(fd, buf, 0, buf.length, start);
      } finally {
        closeSync(fd);
      }
      let s = buf.toString("utf8");
      if (start > 0) {
        const i = s.indexOf("\n"); // drop the partial first line
        if (i >= 0) s = s.slice(i + 1);
      }
      const ls = s.split("\n");
      if (ls[ls.length - 1] === "") ls.pop();
      return ls.slice(-lines).join("\n");
    } catch {
      return "";
    }
  }

  // ── lifecycle ──────────────────────────────────────────────────────

  function updateStatus(): void {
    const running = [...jobs.values()].filter((j) => j.status === "running").length;
    api.ui.setStatus("bg", running ? `background-sessions: ${running} 実行中` : undefined);
  }

  /**
   * Mark a job finished, record it, and hand the notice to `notifyOnce`.
   */
  function finalize(job: Job, code?: number | null): void {
    if (job.status !== "running") return;
    const fresh = readMeta(job.dir);
    const killed = job.killed || fresh?.killed === true;
    job.status = killed ? "killed" : "done";
    // For a job adopted after its exit, the exit-code file's mtime is when it really finished.
    job.finishedAt =
      job.finishedAt ?? (job.child !== undefined ? Date.now() : mtimeOf(job.exitPath) ?? mtimeOf(job.logPath) ?? Date.now());
    job.exitCode = code ?? readExitCode(job.exitPath);
    if (job.spawnError) job.exitCode = null;
    save(job); // the result is on disk even if nobody gets to notify
    updateStatus();
    notifyOnce(job);
  }

  /**
   * Report a finished job exactly once (across processes and sessions: `claimed` is created
   * atomically) with a UI notice plus `session.inject(..., "now")`, which re-fires the agent when
   * it is idle. The session that started the job reports it immediately; other sessions wait
   * `foreignGraceMs` first, so the notice lands in the session that owns the job.
   */
  function notifyOnce(job: Job): void {
    const owner = job.sessionId;
    const foreign = !!owner && !!api.session.id && owner !== api.session.id;

    const claim = (): void => {
      try {
        mkdirSync(join(job.dir, "claimed")); // atomic: one notifier across sasacode processes
      } catch {
        return;
      }
      job.notified = true;
      save(job);

      const elapsed = fmtDur((job.finishedAt ?? Date.now()) - job.startedAt);
      const badge = job.spawnError
        ? `起動に失敗しました (${job.spawnError})`
        : job.exitCode === null || job.exitCode === undefined
          ? "強制終了しました"
          : `exit ${job.exitCode}`;
      api.ui.notify(
        `バックグラウンドジョブ ${job.id} が終了: \`${job.command}\` — ${badge}、実行時間 ${elapsed}`,
        job.exitCode === 0 ? "info" : "warning",
      );
      if (!autoNotify) return;

      const tail = tailOf(job.logPath, tailLines);
      api.session.inject(
        [
          "[background-sessions] バックグラウンドジョブの終了通知",
          foreign ? `（このジョブは別のセッション ${owner} で開始されたものです）` : "",
          `${job.id}: \`${job.command}\` (作業ディレクトリ ${job.runCwd}) — ${badge}、実行時間 ${elapsed}。`,
          `出力ログ: ${job.logPath}`,
          `出力の末尾:\n---\n${tail || "(出力はありません)"}\n---`,
          "この結果を確認し、必要なら修正・再実行・完了報告など次の対応を続けてください。",
        ]
          .filter(Boolean)
          .join("\n"),
        "now",
      );
    };

    if (foreign) {
      // The session that started the job gets first refusal; we only clean up if it is gone.
      const t = setTimeout(claim, foreignGraceMs);
      t.unref?.();
    } else {
      claim();
    }
  }

  function checkRunning(): void {
    for (const job of jobs.values()) {
      if (job.status !== "running") continue;
      if (job.child && job.child.exitCode !== null) continue; // the exit handler finalizes
      if (!isAlive(job.pid)) finalize(job);
    }
  }

  const watch = setInterval(checkRunning, pollMs);
  watch.unref?.();

  function toJob(meta: Meta, dir: string): Job {
    return { ...meta, dir, logPath: logPathOf(dir), exitPath: exitPathOf(dir) };
  }

  /** Adopt jobs from previous runs: keep watching the running ones, report the finished ones. */
  for (const { meta, dir } of diskMetas()) {
    if (meta.cwd !== api.cwd) continue; // another project's session owns it
    if (meta.status !== "running") {
      if (!meta.notified) {
        // It finished while we were away (or a notifier crashed mid-notify): report it now.
        try {
          rmSync(join(dir, "claimed"), { recursive: true, force: true });
        } catch {}
        finalize({ ...toJob(meta, dir), status: "running" }, meta.exitCode);
      }
      continue;
    }
    const job = toJob(meta, dir);
    jobs.set(job.id, job);
    if (!isAlive(job.pid)) finalize(job);
  }
  updateStatus();
  prune();

  /** Keep at most `keep` finished job directories. */
  function prune(): void {
    const finished = diskMetas()
      .filter((m) => m.meta.status !== "running")
      .sort((a, b) => b.meta.startedAt - a.meta.startedAt);
    for (const { dir } of finished.slice(keep)) {
      try {
        rmSync(dir, { recursive: true, force: true });
      } catch {}
    }
  }

  // ── helpers shared by the tools ────────────────────────────────────

  function findJob(id: string): { job?: Job; meta?: Meta; dir: string } {
    const job = jobs.get(id);
    if (job) return { job, meta: job, dir: job.dir };
    for (const { meta, dir } of diskMetas()) if (meta.id === id) return { meta, dir };
    return { dir: "" };
  }

  function jobRow(meta: Meta): string {
    const where = meta.runCwd === meta.cwd ? meta.runCwd : `${meta.runCwd} (session ${meta.cwd})`;
    const label =
      meta.status === "running"
        ? "running"
        : `${meta.status === "killed" ? "killed" : "done"}(${meta.exitCode === null || meta.exitCode === undefined ? "?" : meta.exitCode})`;
    const dur = fmtDur((meta.finishedAt ?? Date.now()) - meta.startedAt);
    return `${meta.id.padEnd(7)} ${label.padEnd(11)} ${dur.padStart(7)}  \`${meta.command}\`  ${where}`;
  }

  function listRows(all: boolean): Meta[] {
    const rows = diskMetas()
      .map((m) => m.meta)
      .filter((m) => all || m.cwd === api.cwd)
      .sort((a, b) => a.startedAt - b.startedAt);
    // include jobs this process started but whose meta write is still racing
    for (const j of jobs.values()) if (!rows.some((m) => m.id === j.id)) rows.push(j);
    return rows.sort((a, b) => a.startedAt - b.startedAt);
  }

  /**
   * SIGTERM to the job's process group, SIGKILL 2 s later if it is still alive. The job is marked
   * killed on disk first, so whichever process sees it exit reports it as killed. False when the
   * process was already gone.
   */
  function stop(m: Meta, job: Job | undefined, dir: string): boolean {
    if (job) job.killed = true;
    else {
      const fresh = readMeta(dir);
      if (fresh) {
        fresh.killed = true;
        try {
          writeFileSync(metaPath(dir), JSON.stringify(fresh, null, 2));
        } catch {}
      }
    }
    const signal = (sig: NodeJS.Signals) => {
      try {
        process.kill(-m.pid, sig); // the job's own process group (spawned detached)
        return true;
      } catch {
        try {
          process.kill(m.pid, sig);
          return true;
        } catch {
          return false;
        }
      }
    };
    if (!signal("SIGTERM")) {
      if (job) finalize(job, null);
      return false;
    }
    const escalate = setTimeout(() => {
      if (isAlive(m.pid)) signal("SIGKILL");
    }, 2000);
    escalate.unref?.();
    updateStatus();
    return true;
  }

  // ── model-facing context ───────────────────────────────────────────

  api.on("system_prompt", (ev) => {
    // Purely a function of which jobs are running: no timestamps or elapsed times, so the prompt
    // (and with it the provider's prompt cache) only changes when a job starts or finishes.
    const running = listRows(false)
      .filter((m) => m.status === "running")
      .sort((a, b) => a.id.localeCompare(b.id, undefined, { numeric: true }));
    if (!running.length) return;
    const lines = running.map((m) => `- ${m.id}: \`${m.command}\``).join("\n");
    return {
      prompt: `${ev.prompt}\n\n## Background jobs (background-sessions plugin)\n実行中のバックグラウンドジョブ:\n${lines}\n終了時は自動的に通知が届きます。ポーリング（sleep ループや繰り返しの確認）はせず、他の作業を進めるかターンを終えてください。`,
    };
  });

  // ── tools ──────────────────────────────────────────────────────────

  const bgStart: ToolDefinition<{ command: string; cwd?: string }> = {
    name: "bg_start",
    description:
      "Start a long-running shell command in the background and return immediately with a job id. " +
      "Its combined stdout/stderr goes to a log file read with bg_output. When the command exits you are notified here automatically with its exit code and output tail, so do not poll it. " +
      "Use for work that takes minutes (builds, test suites, training, downloads, servers you want to keep running).",
    parameters: {
      type: "object",
      properties: {
        command: { type: "string", description: "Shell command to run (executed with bash -c)" },
        cwd: { type: "string", description: "Working directory for the command (default: the session working directory)" },
      },
      required: ["command"],
      additionalProperties: false,
    },
    kind: "exec",
    concurrent: true,
    matchTarget: (a) => a.command,
    // Judged by the rules written for bash (guard preset, the user's bash(sudo *) deny rules,
    // bash(...) allow rules) against the command: no way around them. (plugin API 1.6)
    permissionsAs: "bash",
    summary: (a) => a.command,
    async execute(args, ctx) {
      if (!args.command.trim()) return errorResult("command is empty; pass the shell command to run");
      const runCwd = args.cwd ? (args.cwd.startsWith("/") ? args.cwd : join(ctx.cwd, args.cwd)) : ctx.cwd;
      try {
        if (!statSync(runCwd).isDirectory()) return errorResult(`${runCwd} is not a directory; pass a valid cwd`);
      } catch {
        return errorResult(`working directory ${runCwd} does not exist; create it first or pass a different cwd`);
      }

      // Claim a job directory (`bg-N`) with mkdir: atomic, so ids stay unique across processes.
      let taken: number[] = [];
      try {
        taken = readdirSync(root)
          .map((n) => /^bg-(\d+)$/.exec(n)?.[1])
          .filter((n): n is string => !!n)
          .map(Number);
      } catch {}
      let n = (taken.length ? Math.max(...taken) : 0) + 1;
      let dir = "";
      for (let i = 0; i < 1000; i++, n++) {
        try {
          dir = join(root, `bg-${n}`);
          mkdirSync(dir);
          break;
        } catch {
          dir = "";
        }
      }
      if (!dir) return errorResult(`could not allocate a job directory under ${root}; check disk space and permissions`);

      const job: Job = {
        id: `bg-${n}`,
        command: args.command,
        cwd: ctx.cwd,
        runCwd,
        pid: 0,
        startedAt: Date.now(),
        status: "running",
        sessionId: api.session.id,
        dir,
        logPath: logPathOf(dir),
        exitPath: exitPathOf(dir),
      };
      writeFileSync(job.logPath, "");
      save(job);

      // The wrapper records the exit code to a file via `trap … EXIT`, so even a command that
      // leaves with `exit 3` (or sets its own EXIT trap: it runs in its own bash -c) is recorded
      // and completion is detectable after a restart of sasacode. detached: the job gets its own
      // process group and outlives us.
      const script = `trap 'code=$?; printf "%s\\n" "$code" > "$1"' EXIT\nbash -c "$2"\nexit $?`;
      let child: ReturnType<typeof spawn>;
      const fd = openSync(job.logPath, "a");
      try {
        child = spawn("bash", ["-c", script, "bg-job", job.exitPath, args.command], {
          cwd: runCwd,
          detached: true,
          stdio: ["ignore", fd, fd],
          env: { ...process.env, PAGER: "cat", GIT_PAGER: "cat", SASACODE: "1" },
        });
      } catch (e) {
        closeSync(fd);
        job.spawnError = String(e);
        finalize(job, null);
        return errorResult(`failed to start \`${args.command}\`: ${e}`);
      }
      closeSync(fd);
      job.child = child;
      job.pid = child.pid ?? 0;
      save(job);
      jobs.set(job.id, job);
      child.unref();
      child.on("error", (e) => {
        job.spawnError = String(e);
        finalize(job, null);
      });
      child.on("exit", (code) => finalize(job, code));
      updateStatus();

      return {
        content: [
          text(
            [
              `Started ${job.id} in the background (pid ${job.pid}, in ${runCwd}):`,
              `  $ ${args.command}`,
              `Output log: ${job.logPath}`,
              `A notification with the exit code and the output tail arrives here automatically when it finishes — do not poll it (no sleep loops, no repeated bg_output). Continue with other work, or end your turn and wait for the notice.`,
              api.ui.interactive
                ? undefined
                : "Headless run: when this run ends the process may exit before the notice — it is then delivered at the next sasacode start. If you need the result inside this run, wait in the foreground (e.g. bash `sleep`) and read it with bg_output.",
            ]
              .filter(Boolean)
              .join("\n"),
          ),
        ],
        details: { id: job.id, pid: job.pid, logPath: job.logPath },
      };
    },
  };

  const bgList: ToolDefinition<{ all?: boolean }> = {
    name: "bg_list",
    description:
      "List background jobs started with bg_start (id, status, exit code, elapsed, command). " +
      "Cheap and non-blocking; use it to see what is still running. Running jobs notify you on completion, so this is only for an overview.",
    parameters: {
      type: "object",
      properties: { all: { type: "boolean", description: "Include jobs from other working directories (default false)" } },
      additionalProperties: false,
    },
    kind: "read",
    concurrent: true,
    paths: () => [root],
    summary: () => "background jobs",
    async execute(args) {
      const rows = listRows(args.all === true);
      if (!rows.length) return { content: [text("No background jobs." + (args.all ? "" : " (bg_list all=true shows other directories)"))] };
      const lines = rows.map(jobRow);
      const running = rows.filter((m) => m.status === "running").length;
      lines.push(`[${rows.length} job(s), ${running} running. Running ones notify you automatically when they finish.]`);
      return { content: [text(lines.join("\n"))], details: { jobs: rows } };
    },
  };

  const bgOutput: ToolDefinition<{ id: string; from_line?: number; lines?: number }> = {
    name: "bg_output",
    description:
      "Read the combined stdout/stderr log of a background job started with bg_start. " +
      "Returns the tail by default; pass from_line to page forward from an earlier line. Returns [Showing x-y of z lines] markers when cut.",
    parameters: {
      type: "object",
      properties: {
        id: { type: "string", description: "Job id from bg_start, e.g. bg-3" },
        from_line: { type: "integer", minimum: 1, description: "1-based first line to return (default: the tail)" },
        lines: { type: "integer", minimum: 1, description: `How many lines to return (default ${DEFAULT_PAGE_LINES}, max ${MAX_PAGE_LINES})` },
      },
      required: ["id"],
      additionalProperties: false,
    },
    kind: "read",
    concurrent: true,
    paths: (_a, _cwd) => [root],
    summary: (a) => `output of ${a.id}`,
    async execute(args) {
      const { meta, dir } = findJob(String(args.id));
      if (!meta) return errorResult(`no such job: ${args.id}; use bg_list to see the ids`);
      const path = logPathOf(dir);
      try {
        if (statSync(path).size > MAX_LOG_BYTES) {
          return errorResult(`${path} is over ${MAX_LOG_BYTES / 1024 / 1024}MB; read it with bash (tail/sed/grep) instead`);
        }
      } catch {
        return errorResult(`log of ${meta.id} is missing (${path}); the job may have been pruned`);
      }
      const all = readFileSync(path, "utf8").split("\n");
      if (all[all.length - 1] === "") all.pop();
      const total = all.length;
      const want = Math.min(num(args.lines, DEFAULT_PAGE_LINES), MAX_PAGE_LINES);
      let from: number;
      let to: number;
      let note: string;
      if (args.from_line) {
        from = Math.max(1, Math.min(args.from_line, total + 1)) - 1;
        to = Math.min(from + want, total);
        note = `[Showing lines ${from + 1}-${to} of ${total} of ${meta.id}'s log${meta.status === "running" ? " (job still running)" : ""}. ${
          to < total ? `Use from_line=${to + 1} to continue.` : "End of log."
        } Full log: ${path}]`;
      } else {
        to = total;
        from = Math.max(0, total - want);
        note = `[Showing the last ${to - from} of ${total} lines of ${meta.id}'s log${meta.status === "running" ? " (job still running)" : ""}. ${
          from > 0 ? `Use from_line=1 to read from the start (or from_line=${from + 1 - want} for the previous chunk).` : ""
        } Full log: ${path}]`;
      }
      return {
        content: [text(`${all.slice(from, to).join("\n")}\n${note}`)],
        details: { id: meta.id, status: meta.status, from, to, total },
      };
    },
  };

  const bgKill: ToolDefinition<{ id: string }> = {
    name: "bg_kill",
    description:
      "Stop a background job started with bg_start: SIGTERM to its whole process group, SIGKILL 2 seconds later if it is still alive. " +
      "Use for jobs that are stuck or no longer needed. Only jobs started from the current working directory can be killed. " +
      "A completion notice still arrives when it has exited.",
    parameters: {
      type: "object",
      properties: { id: { type: "string", description: "Job id from bg_start, e.g. bg-3" } },
      required: ["id"],
      additionalProperties: false,
    },
    kind: "exec",
    concurrent: true,
    matchTarget: (a) => a.id,
    summary: (a) => `kill ${a.id}`,
    async execute(args, ctx) {
      const { job, meta, dir } = findJob(String(args.id));
      const m = meta;
      if (!m) return errorResult(`no such job: ${args.id}; use bg_list to see the ids`);
      if (m.cwd !== ctx.cwd) {
        return errorResult(
          `${m.id} was started from ${m.cwd}, not this working directory (${ctx.cwd}); it is not killed here. Open a session in ${m.cwd} to stop it.`,
        );
      }
      if (m.status !== "running") {
        return { content: [text(`${m.id} is already ${m.status} (exit ${m.exitCode ?? "unknown"}); nothing to kill.`)] };
      }
      if (!stop(m, job, dir)) return errorResult(`${m.id}'s process (pid ${m.pid}) is already gone; it will be reported as finished momentarily`);
      return {
        content: [
          text(
            `Sent SIGTERM to the process group of ${m.id} (pid ${m.pid}); SIGKILL follows in 2s if needed. A notice arrives here automatically once it has exited.`,
          ),
        ],
        details: { id: m.id, pid: m.pid },
      };
    },
  };

  api.registerTool(bgStart);
  api.registerTool(bgList);
  api.registerTool(bgOutput);
  api.registerTool(bgKill);
  // Reading our own job logs cannot hurt anything; bg_start / bg_kill keep the normal permission
  // flow (bg_start as bash, via permissionsAs).
  api.permissions.addRules({ allow: ["bg_list", "bg_output"] });

  // ── command: /bg [id | kill <id>] ──────────────────────────────────

  api.registerCommand({
    name: "bg",
    description: "バックグラウンドジョブの一覧・詳細表示・停止",
    argumentHint: "[id] | kill <id>",
    complete: (prefix) => {
      const ids = listRows(false).map((m) => m.id);
      return [...ids, "kill"]
        .filter((v) => v.startsWith(prefix))
        .map((v) => ({ value: v, label: v, description: v === "kill" ? "ジョブを停止" : (jobs.get(v)?.command ?? "") }));
    },
    run({ args }) {
      const parts = args.trim().split(/\s+/).filter(Boolean);
      const rows = listRows(false);
      if (!parts.length) {
        api.ui.notify(
          rows.length
            ? ["バックグラウンドジョブ:", ...rows.map(jobRow), "詳細: /bg <id>　停止: /bg kill <id>"].join("\n")
            : "バックグラウンドジョブはありません",
        );
        return;
      }
      if (parts[0] === "kill") {
        const id = parts[1];
        const meta = rows.find((m) => m.id === id);
        if (!meta) {
          api.ui.notify(`ジョブが見つかりません: ${id}`, "warning");
          return;
        }
        if (meta.status !== "running") {
          api.ui.notify(`${meta.id} は既に終了しています (${meta.status})`);
          return;
        }
        const { job, dir } = findJob(meta.id);
        if (!stop(meta, job, dir)) {
          api.ui.notify(`${meta.id} のプロセスはもう終了しています`);
          return;
        }
        api.ui.notify(`${meta.id} に SIGTERM を送信しました (pid ${meta.pid})`);
        return;
      }
      const meta = rows.find((m) => m.id === parts[0]) ?? (findJob(parts[0]).meta as Meta | undefined);
      if (!meta) {
        api.ui.notify(`ジョブが見つかりません: ${parts[0]}`, "warning");
        return;
      }
      const tail = tailOf(logPathOf(jobs.get(meta.id)?.dir ?? findJob(meta.id).dir), 40);
      api.ui.notify(
        [
          jobRow(meta),
          `出力ログ: ${logPathOf(jobs.get(meta.id)?.dir ?? findJob(meta.id).dir)}`,
          "末尾 40 行:",
          tail || "(出力はありません)",
        ].join("\n"),
      );
    },
  });
};

export default plugin;
