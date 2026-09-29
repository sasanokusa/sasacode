// A stand-in for an OpenAI-compatible model server (vLLM, Ollama, a router): chat completions over
// SSE and /v1/models, driven by a script of replies. It can misbehave the ways real servers do —
// cut a stream short, refuse an effort inside a 200 stream, fail with a status — so end-to-end
// tests exercise the adapters, the HTTP client and the CLI exactly as a real run does.

export interface ToolCallSpec {
  name: string;
  args: Record<string, unknown>;
}

export interface Reply {
  reasoning?: string;
  text?: string;
  toolCalls?: ToolCallSpec[];
  /** Stop sending after the content: no finish_reason, no usage, no [DONE] (a dropped connection). */
  cut?: boolean;
  /** Answer with this HTTP status and an error body instead of a stream. */
  status?: number;
  /** An error event as the first thing in a 200 stream, as vLLM does for an effort it rejects. */
  streamError?: string;
}

export interface FakeServer {
  url: string;
  /** Request bodies in order. */
  requests: any[];
  /** Replies not used yet. */
  left(): number;
  stop(): void;
}

const enc = new TextEncoder();

export function fakeServer(script: Reply[], models: { id: string; max_model_len?: number }[] = [{ id: "m" }]): FakeServer {
  const queue = [...script];
  const requests: any[] = [];
  const server = Bun.serve({
    port: 0,
    async fetch(req) {
      const path = new URL(req.url).pathname;
      if (path.endsWith("/models")) return Response.json({ object: "list", data: models.map((m) => ({ object: "model", owned_by: "fake", ...m })) });
      if (!path.endsWith("/chat/completions")) return new Response("not found", { status: 404 });
      const body = await req.json();
      requests.push(body);
      const reply = queue.shift();
      if (!reply) return Response.json({ error: { message: "the fake server has no more replies" } }, { status: 500 });
      if (reply.status) return Response.json({ error: { message: `fake failure ${reply.status}` } }, { status: reply.status });
      return new Response(stream(body.model, reply), { headers: { "content-type": "text/event-stream" } });
    },
  });
  return { url: `http://127.0.0.1:${server.port}/v1`, requests, left: () => queue.length, stop: () => server.stop(true) };
}

function stream(model: string, r: Reply): ReadableStream {
  const chunk = (delta: object, finish: string | null = null) => ({ id: "c", object: "chat.completion.chunk", created: 0, model, choices: [{ index: 0, delta, finish_reason: finish }] });
  const events: object[] = [];
  if (r.streamError) events.push({ error: { message: r.streamError, type: "BadRequestError", code: 400 } });
  else {
    if (r.reasoning) for (const part of split(r.reasoning)) events.push(chunk({ reasoning_content: part }));
    if (r.text) for (const part of split(r.text)) events.push(chunk({ content: part }));
    r.toolCalls?.forEach((c, i) =>
      events.push(chunk({ tool_calls: [{ index: i, id: `call_${i}_${Math.random().toString(36).slice(2, 8)}`, type: "function", function: { name: c.name, arguments: JSON.stringify(c.args) } }] })),
    );
    if (!r.cut) {
      events.push(chunk({}, r.toolCalls?.length ? "tool_calls" : "stop"));
      events.push({ id: "c", object: "chat.completion.chunk", created: 0, model, choices: [], usage: { prompt_tokens: 100, completion_tokens: 20, total_tokens: 120 } });
    }
  }
  return new ReadableStream({
    async start(controller) {
      for (const e of events) {
        controller.enqueue(enc.encode(`data: ${JSON.stringify(e)}\n\n`));
        await Bun.sleep(1);
      }
      if (!r.cut && !r.streamError) controller.enqueue(enc.encode("data: [DONE]\n\n"));
      controller.close();
    },
  });
}

/** A few pieces, so the client really parses a stream of deltas. */
function split(s: string): string[] {
  const n = Math.max(1, Math.ceil(s.length / 3));
  return [s.slice(0, n), s.slice(n, 2 * n), s.slice(2 * n)].filter(Boolean);
}
