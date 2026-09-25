/**
 * Put text on the system clipboard. A local clipboard tool is tried first because many terminals
 * (macOS Terminal.app, Warp, tmux without passthrough) ignore OSC 52; over SSH the local tools would
 * fill the remote machine's clipboard, so there only the terminal is asked.
 * Returns how it was copied; "terminal" cannot be confirmed.
 */
export async function copyToClipboard(text: string, write: (data: string) => void = (d) => process.stdout.write(d)): Promise<"native" | "terminal"> {
  const remote = !!(process.env.SSH_CONNECTION || process.env.SSH_TTY);
  if (!remote) {
    for (const cmd of clipboardTools()) {
      try {
        // pbcopy decodes its input by the locale and drops non-ASCII text when none is set.
        const env = process.platform === "darwin" ? { ...process.env, LC_ALL: "en_US.UTF-8" } : process.env;
        const p = Bun.spawn(cmd, { stdin: "pipe", stdout: "ignore", stderr: "ignore", env });
        p.stdin.write(text);
        await p.stdin.end();
        if ((await p.exited) === 0) return "native";
      } catch {}
    }
  }
  write(`\x1b]52;c;${Buffer.from(text).toString("base64")}\x07`);
  return "terminal";
}

function clipboardTools(): string[][] {
  if (process.platform === "darwin") return [["pbcopy"]];
  if (process.platform === "win32") return [["clip"]];
  return [["wl-copy"], ["xclip", "-selection", "clipboard"], ["xsel", "--clipboard", "--input"]];
}
