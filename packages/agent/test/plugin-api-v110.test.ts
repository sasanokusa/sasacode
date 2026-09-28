import { expect, test } from "bun:test";
import { PLUGIN_API_VERSION, type PluginAPI } from "@sasacode/plugin-api";
import { Agent, PermissionPolicy, PluginHost, type UIBridge } from "../src/index.ts";

async function api(ui?: Partial<UIBridge>): Promise<PluginAPI> {
  const agent = new Agent({ cwd: "/", systemPrompt: "test", model: { id: "test", provider: "test", api: "replay", contextWindow: 10000, maxOutput: 1000 }, permissions: new PermissionPolicy("edits") });
  const host = new PluginHost({ agent, cwd: "/" });
  if (ui) host.setUI({ interactive: true, notify: () => {}, confirm: async () => false, select: async () => undefined, ...ui });
  let got!: PluginAPI;
  await host.load("p", (a) => { got = a; });
  return got;
}
const opts = [{ value: "a", label: "A" }, { value: "b", label: "B" }, { value: "c", label: "C" }];

test("the host reports API 1.10.0", async () => {
  expect(PLUGIN_API_VERSION).toBe("1.10.0");
  expect((await api()).version).toBe("1.10.0");
});

test("headless input and selectMany resolve undefined without asking", async () => {
  const a = await api();
  expect(await a.ui.input("Name?")).toBeUndefined();
  expect(await a.ui.selectMany("Which?", opts)).toBeUndefined();
});

test("a front end without the dialogs resolves undefined", async () => {
  const a = await api({});
  expect(await a.ui.input("Name?")).toBeUndefined();
  expect(await a.ui.selectMany("Which?", opts)).toBeUndefined();
});

test("input passes title and options through, including an empty answer", async () => {
  const seen: unknown[] = [];
  const a = await api({ input: async (title, o) => { seen.push([title, o]); return ""; } });
  expect(await a.ui.input("Name?", { placeholder: "you", initial: "x" })).toBe("");
  expect(seen).toEqual([["Name?", { placeholder: "you", initial: "x" }]]);
  await expect(a.ui.input("Name?", { placeholder: 3 as never })).rejects.toThrow("input requires");
});

test("selectMany returns only offered values, once each, in option order", async () => {
  const a = await api({ selectMany: async () => ["c", "zzz", "a", "c"] });
  expect(await a.ui.selectMany("Which?", opts, { selected: ["b"] })).toEqual(["a", "c"]);
  const none = await api({ selectMany: async () => [] });
  expect(await none.ui.selectMany("Which?", opts)).toEqual([]);
  await expect(a.ui.selectMany("Which?", [{ value: 1 } as never])).rejects.toThrow("selectMany requires");
  await expect(a.ui.selectMany("Which?", opts, { selected: "a" as never })).rejects.toThrow("selected must be");
});
