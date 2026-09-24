import { afterAll, expect, test } from "bun:test";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Agent, PermissionPolicy, PluginHost } from "@sasacode/agent";
import { readTool } from "@sasacode/tools";
import { createSkillsPlugin, discoverSkills, parseFrontmatter } from "../src/index.ts";

const root = mkdtempSync(join(tmpdir(), "sasacode-skills-"));
afterAll(() => rmSync(root, { recursive: true, force: true }));

function skill(dir: string, name: string, fm: string, body = "Do the thing.") {
  mkdirSync(join(dir, name), { recursive: true });
  writeFileSync(join(dir, name, "SKILL.md"), `---\n${fm}\n---\n\n${body}\n`);
}

test("frontmatter: plain, quoted and folded values", () => {
  expect(parseFrontmatter('---\nname: pdf\ndescription: "Work with PDFs: merge, split"\n---\nbody')).toEqual({
    name: "pdf",
    description: "Work with PDFs: merge, split",
  });
  expect(parseFrontmatter("---\nname: x\ndescription: >\n  line one\n  line two\nlicense: MIT\n---")).toEqual({
    name: "x",
    description: "line one line two",
    license: "MIT",
  });
});

test("discovery: needs name and description; later dirs override", () => {
  const g = join(root, "global");
  const p = join(root, "project");
  skill(g, "a", "name: alpha\ndescription: global alpha");
  skill(g, "b", "name: beta"); // no description: skipped
  skill(p, "a2", "name: alpha\ndescription: project alpha");
  const found = discoverSkills([g, p, join(root, "missing")]);
  expect(found).toEqual([{ name: "alpha", description: "project alpha", path: join(p, "a2", "SKILL.md") }]);
});

test("plugin: lists skills in the system prompt, allows reading them, /skill:<name> injects the body", async () => {
  const dir = join(root, "home-skills");
  skill(dir, "deploy", "name: deploy\ndescription: Deploy the app to staging", "Run make deploy.");
  const agent = new Agent({
    model: { id: "m", provider: "t", api: "replay", contextWindow: 1e5, maxOutput: 1e3 },
    cwd: join(root, "proj"),
    systemPrompt: "base",
    permissions: new PermissionPolicy("edits"),
  });
  const host = new PluginHost({ agent, cwd: agent.cwd });
  await host.load("skills", createSkillsPlugin([dir]));
  const ev = await agent.hooks.run("system_prompt", { prompt: "base" }, (r, e) => {
    if (r.prompt) e.prompt = r.prompt;
  });
  expect(ev.prompt).toContain(`- deploy: Deploy the app to staging (${join(dir, "deploy", "SKILL.md")})`);
  expect(ev.prompt).not.toContain("make deploy"); // body is not preloaded
  const verdict = await agent.permissions.check({ tool: readTool, args: { path: join(dir, "deploy", "SKILL.md") }, cwd: agent.cwd });
  expect(verdict.decision).toBe("allow");
  const cmd = host.commands.find((c) => c.name === "skill:deploy")!;
  await cmd.run({ args: "" });
  await agent.prompt("go"); // queued skill content is delivered with the next prompt
});
