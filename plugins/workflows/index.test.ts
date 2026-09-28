import { expect, test } from "bun:test";
import plugin, { loadWorkflows, renderPrompt } from "./index.ts";
import { loadPlugin } from "../test-support.ts";

test("localized defaults can be extended, overridden and disabled", () => {
  expect(loadWorkflows("ja", {}).get("review")!.prompt).toContain("レビュー");
  expect(loadWorkflows("en", {}).get("review")!.prompt).toContain("Review");
  const w = loadWorkflows("en", { workflows: { review: false, explain: { description: "Explain", prompt: "Explain {{input}}" } } });
  expect(w.has("review")).toBe(false); expect(w.get("explain")!.prompt).toBe("Explain {{input}}");
});

test("template expansion is literal, single-pass and appends when no placeholder", () => {
  const input = "$& {{input}} $(touch nope)\nsecond line";
  expect(renderPrompt({ description: "", prompt: "Review {{input}}" }, input)).toBe(`Review ${input}`);
  expect(renderPrompt({ description: "", prompt: "Review" }, "here")).toBe("Review\n\nhere");
});

test("rejects malformed settings and unsupported placeholders", () => {
  for (const workflows of [[], { show: false }, { bad: { description: "x", prompt: "{{env}}" } }, { bad: { description: "x", prompt: "" } }]) {
    expect(() => loadWorkflows("en", { workflows })).toThrow();
  }
});

test("list/preview never inject, execution injects exactly once and completes names", async () => {
  const h = await loadPlugin("workflows", plugin, process.cwd(), { workflows: { explain: { description: "Explain", prompt: "Explain {{input}}" } } });
  const injected: { content: unknown; when: unknown }[] = [];
  h.agent.inject = (content, when) => { injected.push({ content, when }); };
  const command = h.host.commands.find((c) => c.name === "workflow")!;
  await command.run({ args: "" }); await command.run({ args: "show explain $&\nnext line" });
  expect(injected).toHaveLength(0); expect(h.notices.at(-1)).toBe("/workflow explain\nExplain $&\nnext line");
  await command.run({ args: "missing" }); expect(injected).toHaveLength(0);
  await command.run({ args: "explain login" });
  expect(injected).toEqual([{ content: [{ type: "text", text: "Explain login" }], when: "now" }]);
  expect(await command.complete!("show exp")).toEqual([{ value: "show explain", label: "explain", description: "Explain" }]);
});
