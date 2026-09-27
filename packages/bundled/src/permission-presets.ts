import type { PermissionRules, Plugin } from "@sasacode/plugin-api";

/** Named rule sets, enabled with plugins.settings["permission-presets"].presets (default: guard). */
export const PRESETS: Record<string, { description: string; rules: PermissionRules }> = {
  guard: {
    description:
      "取り返しのつかない操作を常に拒否（sudo、ルートの rm -rf、ディスク操作、パイプで sh に流す等）。ホーム配下の rm -rf と force push は softDeny（jev-guard などが確認に回せる）。ssh・scp・sftp・リモートへの rsync は常に確認",
    rules: {
      deny: [
        "bash(sudo *)",
        "bash(rm -rf /)",
        "bash(rm -rf /*)",
        "bash(mkfs*)",
        "bash(dd *of=/dev/*)",
        "bash(sh)",
        "bash(bash)",
        "bash(zsh)",
        "bash(chmod -R 777 /*)",
      ],
      // Broad patterns that also catch legitimate calls (rm -rf ~/proj/build, a force push to
      // one's own branch): still denied, unless a permission plugin hands them to the user.
      softDeny: ["bash(rm -rf ~*)", "bash(rm -rf $HOME*)", "bash(git push --force*)", "bash(git push -f*)"],
      // Other machines: even a read there is outside what the harness (or Jev, which sees only
      // the command line) can check, so always ask, in auto mode too.
      ask: ["bash(ssh *)", "bash(scp *)", "bash(sftp *)", "bash(rsync *:*)"],
    },
  },
  "read-only-shell": {
    description:
      "読み取りだけのシェルコマンドを確認なしで許可（ls, cat, rg, git status/diff/log など）。書き出しやコマンドの実行をするオプション（rg --pre、git diff --output、tree -o など）は確認に回す",
    rules: {
      allow: [
        "bash(ls*)",
        "bash(pwd)",
        "bash(cat *)",
        "bash(head *)",
        "bash(tail *)",
        "bash(wc *)",
        "bash(rg *)",
        "bash(grep *)",
        "bash(tree*)",
        "bash(file *)",
        "bash(which *)",
        "bash(git status*)",
        "bash(git diff*)",
        "bash(git log*)",
        "bash(git show*)",
        "bash(git branch)",
        "bash(git rev-parse*)",
      ],
    },
  },
  tests: {
    description: "よくあるテスト・型チェックコマンドを確認なしで許可",
    rules: {
      allow: [
        "bash(bun test*)",
        "bash(npm test*)",
        "bash(pnpm test*)",
        "bash(yarn test*)",
        "bash(cargo test*)",
        "bash(go test*)",
        "bash(pytest*)",
        "bash(tsc --noEmit*)",
        "bash(bun run typecheck*)",
      ],
    },
  },
};

/**
 * Options that turn a read-only-shell command into one that writes files or runs programs. The
 * allow rules match by prefix, so these are handed back to the user. Tokens are compared without
 * quotes and backslashes; git also takes unambiguous prefixes of long options (--outp=…).
 */
const WRITES_OR_RUNS: [RegExp, RegExp][] = [
  [/^rg$/, /^--(pre|hostname-bin)(=|$)/],
  [/^git (diff|log|show)$/, /^--(ou|ex)/],
  [/^tree$/, /^(-[a-zA-Z]*[oR]|--(o|fromfile))/],
  [/^file$/, /^(-[a-zA-Z]*C|--c)/],
];

export function writesOrRuns(command: string): boolean {
  return command.split(/&&|\|\||;|\||\n|&/).some((piece) => {
    const words = piece.trim().replace(/["'\\]/g, "").split(/\s+/);
    return WRITES_OR_RUNS.some(([cmd, opt]) => {
      const name = cmd.test(words.slice(0, 2).join(" ")) ? 2 : cmd.test(words[0] ?? "") ? 1 : 0;
      return name > 0 && words.slice(name).some((w) => opt.test(w));
    });
  });
}

const permissionPresets: Plugin = (api) => {
  const enabled = (api.settings.presets as string[] | undefined) ?? ["guard"];
  for (const name of enabled) {
    const p = PRESETS[name];
    if (!p) api.ui.notify(`unknown permission preset "${name}" (${Object.keys(PRESETS).join(", ")})`, "warning");
    else api.permissions.addRules(p.rules);
  }
  if (enabled.includes("read-only-shell"))
    api.on("permission", ({ tool, args, verdict }) => {
      // Only calls a rule allowed: auto mode (and the user's own asks) are left as they are.
      if (tool.name !== "bash" || verdict.decision !== "allow" || verdict.source !== "rule") return;
      if (writesOrRuns(String(args.command ?? "")))
        return { decision: "ask", reason: "read-only-shell: this option writes files or runs programs" };
    });
  api.registerCommand({
    name: "presets",
    description: "権限プリセットの一覧",
    run: () =>
      api.ui.notify(
        Object.entries(PRESETS)
          .map(([n, p]) => `${enabled.includes(n) ? "●" : "○"} ${n} — ${p.description}`)
          .concat('有効化: config の plugins.settings["permission-presets"].presets')
          .join("\n"),
      ),
  });
};
export default permissionPresets;
