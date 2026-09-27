import { fstatSync } from "node:fs";

/** How long a pipe may stay silent before it is taken for one nobody writes to (without --stdin). */
const PIPE_WAIT_MS = 5000;

/**
 * Piped input is appended to -p. A file is read whole. A pipe is read to its end once something
 * arrives; one that stays open and silent (a parent shell's) is left alone after PIPE_WAIT_MS,
 * with a note, unless `wait` (--stdin) says the input is coming however long it takes.
 */
export async function readPipedStdin(wait = false, fd = 0): Promise<string> {
  if (fd === 0 && process.stdin.isTTY) return "";
  let file = false;
  try {
    const st = fstatSync(fd);
    if (st.isCharacterDevice()) return ""; // a terminal or /dev/null
    file = st.isFile();
  } catch {
    return "";
  }
  const reader = (fd === 0 ? Bun.stdin : Bun.file(fd)).stream().getReader();
  let timer: ReturnType<typeof setTimeout> | undefined;
  const silent = new Promise<undefined>((r) => (timer = setTimeout(() => r(undefined), PIPE_WAIT_MS)));
  const first = await (wait || file ? reader.read() : Promise.race([reader.read(), silent]));
  clearTimeout(timer);
  if (!first || first.done) {
    reader.releaseLock();
    if (!first) console.error(`warning: nothing arrived on stdin within ${PIPE_WAIT_MS / 1000}s, so it was not read (pass --stdin to wait for it)`);
    return "";
  }
  const chunks = [first.value];
  for (let r = await reader.read(); !r.done; r = await reader.read()) chunks.push(r.value);
  return Buffer.concat(chunks).toString("utf8");
}
