#!/usr/bin/env node
// The stdout pipe is a private protocol, not a log. Never forward its raw contents to diagnostics.
import { SyncEngine, safeError } from "./engine.mjs";
import { MAX_PROTOCOL_BYTES } from "./worker.mjs";
const pending = new Map();
let sequence = 0,
  queue = Promise.resolve(),
  buffer = "";
const write = (value) => process.stdout.write(`${JSON.stringify(value)}\n`);
const engine = new SyncEngine(
  (target, request) =>
    new Promise((resolve, reject) => {
      const transportId = String(++sequence);
      // Rust owns connection/worker deadlines and cancellation. An independent
      // timer here could discard an apply while its native authentication dialog
      // is still open, then let the late authenticated request write unnoticed.
      pending.set(transportId, { resolve, reject });
      write({ type: "transport", transportId, target, request });
    }),
);
function redact(value, key = "") {
  if (/password|secret|token|api.?key|authorization|credential/i.test(key))
    return "[REDACTED]";
  if (Array.isArray(value)) return value.map((v) => redact(v, key));
  if (value && typeof value === "object")
    return Object.fromEntries(
      Object.entries(value).map(([k, v]) => [k, redact(v, k)]),
    );
  if (typeof value === "string") {
    if (
      /(?:bearer\s|sk-[A-Za-z0-9]|(?:api[_-]?key|token|password|secret)\s*[:=]|https?:\/\/[^/\s]*@)/i.test(
        value,
      )
    )
      return "[REDACTED]";
    return value.replace(/([?&](?:[^=&#]+)=)[^&#\s]+/g, "$1[REDACTED]");
  }
  return value;
}
function review(result) {
  // Commands/arguments can contain secrets even without recognizable token patterns.
  for (const target of result.targets ?? [])
    for (const [name, doc] of Object.entries(target.documents ?? {})) {
      for (const row of [...doc.changes, ...doc.conflicts]) {
        for (const key of ["before", "after", "source", "target", "base"])
          if (row[key]) {
            row[key] =
              name === "mcp" &&
              /\/(command|args)$/.test(row.path) &&
              row[key].present
                ? { present: true, value: "[REDACTED: command or arguments]" }
                : redact(row[key]);
          }
      }
    }
  for (const target of result.targets ?? [])
    for (const file of target.files ?? []) {
      file.before = redact(file.before);
      file.after = redact(file.after);
    }
  return result;
}
async function dispatch(message) {
  const id =
    typeof message.id === "string" || typeof message.id === "number"
      ? message.id
      : null;
  try {
    let result;
    switch (message.op) {
      case "contents":
        result = await engine.contents(message);
        break;
      case "preview":
        result = review(await engine.preview(message));
        break;
      case "resolve":
        result = review(engine.resolve(message));
        break;
      case "reveal": {
        const captured = engine.reveal(message);
        result = review(structuredClone(captured));
        for (const target of result.targets) {
          const original = captured.targets.find((t) => t.id === target.id);
          if (original.documents?.mcp)
            target.documents.mcp = original.documents.mcp;
        }
        break;
      }
      case "review-file":
        // This response alone intentionally bypasses redaction after the handler
        // verifies explicit approval and the captured selected file scope.
        result = await engine.reviewFile(message);
        break;
      case "apply":
        result = await engine.apply(message);
        break;
      case "discard":
        result = engine.discard(message.planId);
        break;
      case "ping":
        result = { protocolVersion: 1, node: process.versions.node };
        break;
      default:
        throw new Error("INVALID_OPERATION");
    }
    write({ id, ok: true, ...result });
  } catch (e) {
    write({ id, ok: false, error: { code: safeError(e), ...(e.errorDetail ? { detail: e.errorDetail } : {}) } });
  }
}
function line(text) {
  let message;
  try {
    message = JSON.parse(text);
    if (!message || typeof message !== "object" || Array.isArray(message))
      throw Error();
  } catch {
    write({ id: null, ok: false, error: { code: "INVALID_JSON_REQUEST" } });
    return;
  }
  if (message.op === "transport-result") {
    const p = pending.get(String(message.transportId));
    if (!p) return;
    pending.delete(String(message.transportId));
    if (message.error) p.reject(new Error("SSH_FAILED"));
    else p.resolve(message.response);
    return;
  }
  queue = queue.then(() => dispatch(message));
}
process.stdin.setEncoding("utf8");
process.stdin.on("data", (chunk) => {
  buffer += chunk;
  let end;
  while ((end = buffer.indexOf("\n")) !== -1) {
    const next = buffer.slice(0, end);
    buffer = buffer.slice(end + 1);
    if (Buffer.byteLength(next) > MAX_PROTOCOL_BYTES) {
      write({ id: null, ok: false, error: { code: "REQUEST_TOO_LARGE" } });
      continue;
    }
    if (next.trim()) line(next);
  }
  if (Buffer.byteLength(buffer) > MAX_PROTOCOL_BYTES) {
    write({ id: null, ok: false, error: { code: "REQUEST_TOO_LARGE" } });
    process.exit(2);
  }
});
process.stdin.on("end", () => {
  if (buffer.trim()) line(buffer);
  for (const p of pending.values()) {
    p.reject(new Error("TRANSPORT_CLOSED"));
  }
  pending.clear();
});
process.stdout.on("error", () => process.exit(1));
