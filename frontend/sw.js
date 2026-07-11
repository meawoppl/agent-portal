// Agent Portal service worker (PWA baseline — plan item A2).
//
// Strategy:
//   - navigation requests / index.html: network-first, fall back to cache when
//     offline (so a backend deploy is picked up immediately, but the installed
//     app still opens with no network).
//   - Trunk's hashed assets (*.wasm / *.js / *.css — filenames carry a content
//     hash): cache-first (immutable, safe to serve from cache forever).
//   - everything else (images, fonts, cross-origin CDN): pass through to the
//     network untouched.
//
// NEVER intercepted: /api/*, /ws/*, and any non-GET request. Those must always
// hit the network directly — caching them would break auth, WS upgrades, and
// mutations.
//
// Cache versioning: the app registers this worker as `/sw.js?v=<shared::VERSION>`
// (see frontend/src/lib.rs). The version travels in `self.location.search`, so a
// new deploy => a new cache name => stale caches are dropped on `activate`. This
// is the guard against serving stale WASM after a deploy (§12 of the mobile plan).

const CACHE_NAME = "agent-portal" + self.location.search;
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
  return /\.(wasm|js|css)$/.test(url.pathname);
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
      fetch(request)
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
