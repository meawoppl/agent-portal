// Runs the real frontend/sw.js push handler against a stubbed service-worker
// global, with payloads shaped like the backend's `WebPushBody`
// (backend/src/push/webpush.rs). The tag is what makes a newer notification
// replace an older one for the same session; reading the wrong field (#2238)
// leaves it unset, and notifications stack.
const test = require("node:test");
const assert = require("node:assert");
const fs = require("node:fs");
const path = require("node:path");
const vm = require("node:vm");

function loadWorker() {
  const handlers = {};
  const shown = [];
  const self = {
    addEventListener: (type, fn) => {
      handlers[type] = fn;
    },
    registration: {
      showNotification: async (title, options) => shown.push({ title, options }),
    },
    clients: { claim: async () => {}, matchAll: async () => [] },
    skipWaiting: () => {},
    location: { origin: "https://portal.example" },
  };
  const source = fs.readFileSync(path.join(__dirname, "..", "sw.js"), "utf8");
  vm.runInNewContext(source, { self, caches: {}, fetch: async () => {}, URL, console });
  const push = (payload) =>
    new Promise((resolve) =>
      handlers.push({
        data: { json: () => payload, text: () => JSON.stringify(payload) },
        waitUntil: (work) => Promise.resolve(work).then(resolve),
      }),
    );
  return { push, shown };
}

const wire = (session, body) => ({
  session_id: session,
  event_kind: "awaiting_input",
  title: "Agent",
  body,
  tag: session,
});

test("notifications for one session share a tag so the newest replaces", async () => {
  const worker = loadWorker();
  await worker.push(wire("s-1", "first"));
  await worker.push(wire("s-1", "second"));
  assert.deepStrictEqual(
    worker.shown.map((n) => n.options.tag),
    ["s-1", "s-1"],
  );
});

test("different sessions keep distinct tags", async () => {
  const worker = loadWorker();
  await worker.push(wire("s-1", "a"));
  await worker.push(wire("s-2", "b"));
  assert.deepStrictEqual(
    worker.shown.map((n) => n.options.tag),
    ["s-1", "s-2"],
  );
});

test("the session id stays on the notification for click routing", async () => {
  const worker = loadWorker();
  await worker.push(wire("s-9", "x"));
  assert.strictEqual(worker.shown[0].options.data.session_id, "s-9");
  assert.strictEqual(worker.shown[0].options.body, "x");
});
