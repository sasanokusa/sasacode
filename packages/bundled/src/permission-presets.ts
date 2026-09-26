import type { PermissionRules, Plugin } from "@sasacode/plugin-api";

/** Named rule sets, enabled with plugins.settings["permission-presets"].presets (default: guard). */
export const PRESETS: Record<string, { description: string; rules: PermissionRules }> = {
  guard: {
    description:
      "取り返しのつかない操作を常に拒否（sudo、ルートの rm -rf、ディスク操作、パイプで sh に流す等）。ホーム配下の rm -rf と force push は softDeny（jev-guard などが確認に回せる）",
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
    },
  },
  "read-only-shell": {
    description: "読み取りだけのシェルコマンドを確認なしで許可（ls, cat, rg, git status/diff/log など）",
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

const permissionPresets: Plugin = (api) => {
  const enabled = (api.settings.presets as string[] | undefined) ?? ["guard"];
  for (const name of enabled) {
    const p = PRESETS[name];
    if (!p) api.ui.notify(`unknown permission preset "${name}" (${Object.keys(PRESETS).join(", ")})`, "warning");
    else api.permissions.addRules(p.rules);
  }
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
