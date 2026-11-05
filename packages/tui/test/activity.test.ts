import { expect, test } from "bun:test";
import type { TUI } from "@earendil-works/pi-tui";
import { Activity, fmtRate } from "../src/activity.ts";

const ui = { requestRender() {} } as unknown as TUI;
const id = (s: string) => s;

/** The activity line at a clock the test moves, re-rendered (the line itself only updates each tick). */
function line(clock: { now: number }) {
  const a = new Activity(ui, id, id, Math.random, () => clock.now);
  const text = () => {
    a.setDetail(undefined);
    return a.render(200).join("\n");
  };
  return { a, text };
}

test("fmtRate: one decimal below 10 a second, whole numbers above", () => {
  expect(fmtRate(3.14)).toBe("3.1");
  expect(fmtRate(42.6)).toBe("43");
});

test("tok/s: measured from the first delta, shown once half a second has streamed", () => {
  const clock = { now: 0 };
  const { a, text } = line(clock);
  a.newReply();
  clock.now = 3000; // waiting for the first delta (the prompt being read) does not count
  a.streamed("x".repeat(400)); // 100 tokens
  clock.now = 3200;
  a.streamed("x".repeat(400));
  expect(text()).not.toContain("tok/s"); // 0.2 s: too short to tell a pace from a burst
  clock.now = 4000;
  a.streamed("x".repeat(400)); // 300 tokens in 1 s
  expect(text()).toContain("300 tok/s");
  a.stop();
});

test("tok/s: the provider's count replaces the estimate at the end, and stays while tools run", () => {
  const clock = { now: 0 };
  const { a, text } = line(clock);
  a.newReply();
  a.streamed("x".repeat(400)); // estimated 100
  clock.now = 2000;
  a.replyDone(120); // the real count, over 2 s
  expect(text()).toContain("60 tok/s");
  a.setDetail("read を実行中…");
  clock.now = 9000;
  expect(a.render(200).join("\n")).toContain("60 tok/s");
  a.stop();
});

test("tok/s: a count far above what streamed (hidden reasoning) falls back to the estimate", () => {
  const clock = { now: 0 };
  const { a, text } = line(clock);
  a.newReply();
  a.streamed("x".repeat(400)); // estimated 100
  clock.now = 1000;
  a.replyDone(5000); // reasoning made before the first delta, in no measured time
  expect(text()).toContain("100 tok/s");
  a.stop();
});
