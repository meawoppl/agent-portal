// Agent Portal service worker (PWA baseline — plan item A2).
//
// Strategy:
//   - navigation requests / index.html: network-first, fall back to cache when
//     offline (so a backend deploy is picked up immediately, but the installed
//     app still opens with no network).
//   - Trunk's hashed assets (*.wasm / *.js / *.css whose filename ends in a
//     content hash, e.g. base-545c8725d321f26d.css): cache-first (immutable,
//     safe to serve from cache forever).
//   - everything else — including stable-URL helper scripts like katex-helper.js
//     — passes through to the network, so a changed file never fails the
//     integrity check in a fresh index.html.
//
// NEVER intercepted: /api/*, /ws/*, and any non-GET request. Those must always
// hit the network directly — caching them would break auth, WS upgrades, and
// mutations.
//
// Hashed application assets are safe to retain across deploys. The app shell is
// always fetched network-first with the HTTP cache bypassed, and activation
// removes cache layouts created by older workers.

const CACHE_NAME = "agent-portal-bootstrap-v3";
const APP_SHELL = "/index.html";

// Precache the app shell so navigations work offline right after install.
self.addEventListener("install", (event) => {
  event.waitUntil(
    caches
      .open(CACHE_NAME)
      .then((cache) => cache.add(APP_SHELL))
      .catch(() => {})
      .then(() => self.skipWaiting()),
  );
});

// Drop caches from previous versions and take control immediately.
self.addEventListener("activate", (event) => {
  event.waitUntil(
    caches
      .keys()
      .then((keys) =>
        Promise.all(
          keys
            .filter((k) => k.startsWith("agent-portal") && k !== CACHE_NAME)
            .map((k) => caches.delete(k)),
        ),
      )
      .then(() => self.clients.claim()),
  );
});

function isHashedAsset(url) {
  // Keep in sync with is_content_hashed_asset in backend/src/routes.rs.
  return /\/[^/]+-[0-9a-f]{12,16}(_bg)?\.(wasm|js|css)$/.test(url.pathname);
}

self.addEventListener("fetch", (event) => {
  const request = event.request;

  // Only ever touch GET requests.
  if (request.method !== "GET") {
    return;
  }

  const url = new URL(request.url);

  // Same-origin only — let cross-origin (CDN, etc.) go straight to the network.
  if (url.origin !== self.location.origin) {
    return;
  }

  // Never intercept the API or WebSocket surfaces.
  if (url.pathname.startsWith("/api/") || url.pathname.startsWith("/ws/")) {
    return;
  }

  // Navigations and the app shell: network-first, cache fallback.
  if (request.mode === "navigate" || url.pathname === APP_SHELL) {
    event.respondWith(
      fetch(new Request(request, { cache: "no-store" }))
        .then((response) => {
          const copy = response.clone();
          caches
            .open(CACHE_NAME)
            .then((cache) => cache.put(APP_SHELL, copy))
            .catch(() => {});
          return response;
        })
        .catch(() =>
          caches
            .match(APP_SHELL, { ignoreSearch: true })
            .then((cached) => cached || Response.error()),
        ),
    );
    return;
  }

  // Hashed, immutable assets: cache-first.
  if (isHashedAsset(url)) {
    event.respondWith(
      caches.match(request).then((cached) => {
        if (cached) {
          return cached;
        }
        return fetch(request).then((response) => {
          if (response && response.ok) {
            const copy = response.clone();
            caches
              .open(CACHE_NAME)
              .then((cache) => cache.put(request, copy))
              .catch(() => {});
          }
          return response;
        });
      }),
    );
  }

  // Everything else: default network handling (no respondWith).
});

// ---------------------------------------------------------------------------
// Web Push (mobile-apps plan D1).
//
// Payload contract (JSON, `WebPushBody` in backend/src/push/webpush.rs): {
//   session_id, event_kind, title, body, tag }. `tag` (the session's collapse
// key) becomes the notification `tag`, so one visible notification per session
// is shown (newest wins) rather than a growing stack.
// Everything is best-effort and defensive: a malformed/empty payload still
// surfaces a generic notification rather than throwing inside the SW.
// ---------------------------------------------------------------------------
self.addEventListener("push", (event) => {
  let payload = {};
  if (event.data) {
    try {
      payload = event.data.json() || {};
    } catch (e) {
      // Non-JSON payload — fall back to the raw text as the body.
      payload = { body: event.data.text() };
    }
  }

  const title = payload.title || "Agent Portal";
  const options = {
    body: payload.body || "",
    tag: payload.tag || undefined,
    icon: "/icon-192.png",
    // Android masks the badge to a monochrome silhouette. Keep its background
    // transparent: the opaque launcher icon would become a solid square.
    badge: "/notification-badge-96.png",
    data: { session_id: payload.event_kind === "test" ? null : (payload.session_id || null) },
  };

  event.waitUntil(self.registration.showNotification(title, options));
});

// Ask the app to route inside its existing document. In particular, do not
// focus an arbitrary Chrome tab or force a WindowClient.navigate() over the
// installed Android app's working viewport. Older/loading pages may not have
// a listener, so every request is bounded and falls back to openWindow().
function askClient(client, message) {
  return new Promise((resolve) => {
    const channel = new MessageChannel();
    const finish = (value) => {
      clearTimeout(timer);
      channel.port1.close();
      channel.port2.close();
      resolve(value);
    };
    const timer = setTimeout(() => finish(null), 500);
    channel.port1.onmessage = (event) => finish(event.data);
    try {
      client.postMessage(message, [channel.port2]);
    } catch (_) {
      finish(null);
    }
  });
}

async function openNotification(sessionId) {
  const target = new URL("/dashboard", self.location.origin);
  if (sessionId) target.searchParams.set("session", sessionId);

  const clients = await self.clients.matchAll({ type: "window", includeUncontrolled: true });
  const candidates = await Promise.all(clients
    .filter((client) => client.frameType === "top-level" &&
      new URL(client.url).origin === self.location.origin)
    .map(async (client) => ({
      client,
      standalone: await askClient(client, { type: "portal-notification-client" }),
    })));

  // matchAll() orders by recent focus, which may put a browser tab ahead of
  // the home-screen app. Prefer the installed app; retain recency within each
  // group. Only reuse pages that confirmed they understand this protocol.
  candidates.sort((a, b) => Number(b.standalone === true) - Number(a.standalone === true));
  for (const { client, standalone } of candidates) {
    if (typeof standalone !== "boolean") continue;
    try {
      await client.focus();
      if (await askClient(client, { type: "portal-open-session", session_id: sessionId }) === true) {
        return;
      }
    } catch (_) {
      // The window may have closed between enumeration and focus.
    }
  }
  // Chrome can launch/reuse the installed app for this in-scope URL. This
  // also loads the current app when a stale page cannot handle the message.
  return self.clients.openWindow(target.href);
}

self.addEventListener("notificationclick", (event) => {
  event.notification.close();
  const sessionId =
    event.notification.data?.session_id || null;
  event.waitUntil(openNotification(sessionId));
});
