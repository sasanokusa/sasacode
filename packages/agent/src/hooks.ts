import type { HookHandler, HookMap, HookName } from "@sasacode/plugin-api";

type Step<K extends HookName> = (result: HookMap[K]["result"], event: HookMap[K]["event"], owner: string) => boolean | void;

interface Entry {
  owner: string;
  fn: (event: any) => any;
}

/** Runs hook handlers in registration order. A failing handler is reported and skipped; the loop goes on. */
export class HookRunner {
  private handlers = new Map<HookName, Entry[]>();
  onError?: (owner: string, hook: HookName, error: unknown) => void;

  on<K extends HookName>(name: K, fn: HookHandler<K>, owner = "core"): void {
    const list = this.handlers.get(name) ?? [];
    list.push({ owner, fn });
    this.handlers.set(name, list);
  }

  /** A runner sharing these handlers except the named hooks (for subagents). */
  without(names: HookName[]): HookRunner {
    const r = new HookRunner();
    for (const [k, v] of this.handlers) if (!names.includes(k)) r.handlers.set(k, v);
    r.onError = this.onError;
    return r;
  }

  has(name: HookName): boolean {
    return !!this.handlers.get(name)?.length;
  }

  /**
   * Call each handler with the current event; `step` folds its result into the event and
   * returns false to stop early.
   */
  async run<K extends HookName>(name: K, event: HookMap[K]["event"], step?: Step<K>): Promise<HookMap[K]["event"]> {
    for (const h of this.handlers.get(name) ?? []) {
      let result: HookMap[K]["result"] | void;
      try {
        result = await h.fn(event);
      } catch (e) {
        this.onError?.(h.owner, name, e);
        continue;
      }
      if (result && step && step(result, event, h.owner) === false) break;
    }
    return event;
  }

  /** For hooks on the streaming path: handlers must not return a promise. */
  runSync<K extends HookName>(name: K, event: HookMap[K]["event"], step?: Step<K>): void {
    for (const h of this.handlers.get(name) ?? []) {
      let result: unknown;
      try {
        result = h.fn(event);
      } catch (e) {
        this.onError?.(h.owner, name, e);
        continue;
      }
      if (result instanceof Promise) {
        result.catch(() => {});
        this.onError?.(h.owner, name, new Error(`${name} handlers must be synchronous; the result was ignored`));
        continue;
      }
      if (result && step && step(result as HookMap[K]["result"], event, h.owner) === false) break;
    }
  }
}
