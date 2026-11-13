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

function loadWorker(clients = []) {
  const handlers = {};
  const shown = [];
  const self = {
    addEventListener: (type, fn) => {
      handlers[type] = fn;
    },
    registration: {
      showNotification: async (title, options) => shown.push({ title, options }),
    },
    clients: {
      claim: async () => {},
      matchAll: async () => clients,
      openWindow: async (url) => opened.push(url),
    },
    skipWaiting: () => {},
    location: { origin: "https://portal.example" },
  };
  const source = fs.readFileSync(path.join(__dirname, "..", "sw.js"), "utf8");
  const opened = [];
  vm.runInNewContext(source, {
    self, caches: {}, fetch: async () => {}, URL, console,
    MessageChannel, setTimeout, clearTimeout,
  });
  const push = (payload) =>
    new Promise((resolve) =>
      handlers.push({
        data: { json: () => payload, text: () => JSON.stringify(payload) },
        waitUntil: (work) => Promise.resolve(work).then(resolve),
      }),
    );
  const click = (session_id) => new Promise((resolve, reject) => {
    handlers.notificationclick({
      notification: { data: { session_id }, close: () => {} },
      waitUntil: (work) => Promise.resolve(work).then(resolve, reject),
    });
  });
  return { push, shown, click, opened };
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


test("test notifications have no fake session destination", async () => {
  const worker = loadWorker();
  await worker.push({ ...wire("00000000-0000-0000-0000-000000000000", "test"), event_kind: "test" });
  assert.equal(worker.shown[0].options.data.session_id, null);
});

function appClient({ standalone = false, responds = true, focusFails = false,
  accepts = true, frameType = "top-level", origin = "https://portal.example" } = {}) {
  const received = [];
  let focused = 0;
  let navigated = 0;
  const client = {
    url: `${origin}/dashboard`, frameType,
    focus: async () => {
      if (focusFails) throw new Error("Window closed");
      focused++;
      return client;
    },
    navigate: async () => { navigated++; return client; },
    postMessage: (message, [port]) => {
      received.push(message);
      if (responds) port.postMessage(message.type === "portal-notification-client" ? standalone : accepts);
    },
  };
  return { client, received, focused: () => focused, navigated: () => navigated };
}

test("notification prefers the installed app over a more recently focused browser tab", async () => {
  const tab = appClient();
  const app = appClient({ standalone: true });
  const worker = loadWorker([tab.client, app.client]);
  await worker.click("s-9");
  assert.equal(tab.focused(), 0);
  assert.equal(app.focused(), 1);
  assert.equal(app.navigated(), 0, "must retain the app's mobile document");
  assert.equal(app.received.at(-1).session_id, "s-9");
  assert.deepEqual(worker.opened, []);
});

test("a browser-only session is reused without reload on successive notification taps", async () => {
  const tab = appClient();
  const worker = loadWorker([tab.client]);
  await worker.click("s-1");
  await worker.click("s-2");
  assert.deepEqual(tab.received.filter(m => m.type === "portal-open-session").map(m => m.session_id), ["s-1", "s-2"]);
  assert.equal(tab.navigated(), 0);
  assert.deepEqual(worker.opened, []);
});

test("a closed installed app does not swallow the click", async () => {
  const app = appClient({ standalone: true, focusFails: true });
  const worker = loadWorker([app.client]);
  await worker.click("s-1");
  assert.deepEqual(worker.opened, ["https://portal.example/dashboard?session=s-1"]);
});

test("stale or nonresponsive pages fall back to an in-scope app launch", async () => {
  const old = appClient({ responds: false });
  const worker = loadWorker([old.client]);
  await worker.click("s-1");
  assert.equal(old.focused(), 0);
  assert.deepEqual(worker.opened, ["https://portal.example/dashboard?session=s-1"]);
});

test("routing must be acknowledged before a notification click is considered handled", async () => {
  const app = appClient({ standalone: true, accepts: false });
  const worker = loadWorker([app.client]);
  await worker.click("s-1");
  assert.deepEqual(worker.opened, ["https://portal.example/dashboard?session=s-1"]);
});

test("iframes and foreign-origin pages cannot take notification launches", async () => {
  const frame = appClient({ frameType: "nested", standalone: true });
  const foreign = appClient({ origin: "https://other.example", standalone: true });
  const worker = loadWorker([frame.client, foreign.client]);
  await worker.click(null);
  assert.equal(frame.received.length + foreign.received.length, 0);
  assert.deepEqual(worker.opened, ["https://portal.example/dashboard"]);
});

test("cold notification launches preserve the session without query injection", async () => {
  const worker = loadWorker();
  await worker.click("s-1&other=x");
  const target = new URL(worker.opened[0]);
  assert.equal(target.searchParams.get("session"), "s-1&other=x");
  assert.equal(target.searchParams.size, 1);
});
