// A dead session goes to the login page — one definition of what
// that means, for every caller that learns a session is dead.
//
// Three places need the same answer, and each used to spell it out:
//   - the fetch guard App.svelte installs (`guardFetch` below; it was
//     an inline interceptor there until backlog 7d70cad6), for /api/*
//     reads and writes (identity probes exempt — a 401 on "who am I"
//     is the ANSWER, and redirecting on it made `unauthenticated`
//     unreachable), and for Access expiry at the edge, which arrives
//     as a thrown fetch rather than a status.
//   - steps/pluginHost.ts, for the `/plugins/<bundle>` preflight. The
//     interceptor never covered it, so an expired session rendered a
//     plugin-load error with a Retry button that could not succeed:
//     the surface reported a symptom while the system already held
//     the cause (David, 2026-09-10).
//   - ui/WriteGate.svelte's "sign in to act" href, for the read-only
//     guest — a link rather than a redirect, but the same target and
//     the same ?next=.
//
// The redirect target, the ?next= encoding and the on-/login guard
// are one fact, so they live here once (CLAUDE.md §9a — collapse it
// if you can; a comment asking call sites to agree is not a
// mechanism).

/** The login route, as the router spells it (`router.ts` matches the
 *  literal `/login`, and LoginPage reads `?next=` back off it). */
export const LOGIN_PATH = '/login';

/** Where a dead session should land: the login page with the page the
 *  operator was reading captured as `?next=`, so signing in returns
 *  them there. LoginPage admits in-app paths only, so the encoding
 *  here is the whole contract. Pure — takes the location rather than
 *  reading one. */
export function loginUrlFor(pathname: string, search: string): string {
  return `${LOGIN_PATH}?next=${encodeURIComponent(pathname + search)}`;
}

/** The slice of `window` a redirect needs. Declared locally so tests
 *  can pass a plain object and so nothing here depends on a DOM. */
export type NavWindow = {
  location: { pathname: string; search: string; href: string };
};

// Once-per-page latch. A dead session fails EVERY in-flight request,
// and a step page can have several plugin preflights and several API
// reads outstanding at the same moment; without this, each one
// restates the navigation. The first call captures `next` and wins.
// Module-level because a page's "I have already left" is a property
// of the page, not of any one caller; `_resetDeadSessionForTests`
// clears it the way pluginHost clears its caches.
let alreadyLeaving = false;

function currentWindow(): NavWindow | null {
  return (globalThis as { window?: NavWindow }).window ?? null;
}

/** Send this tab to the login page. Returns whether it issued the
 *  navigation, so a caller can tell "handled, stop explaining this
 *  failure" from "nothing happened, say what you know".
 *
 *  Refuses in two cases, both of which would be a loop or noise:
 *  already on /login (the page that fixes it), and already leaving. */
export function goToLogin(win: NavWindow | null = currentWindow()): boolean {
  if (!win) return false;
  if (win.location.pathname === LOGIN_PATH) return false;
  if (alreadyLeaving) return false;
  alreadyLeaving = true;
  win.location.href = loginUrlFor(win.location.pathname, win.location.search);
  return true;
}

// ---------------------------------------------------------------------
// Access expiry at the edge (backlog 7d70cad6, David 2026-11-16).
//
// The deployment sits behind Cloudflare Access, which keeps its OWN
// session in front of the gateway's. When it lapses, Access answers a
// read with a 302 to its login origin. A document navigation follows
// that fine: Access puts the url in `redirect_url` and returns the
// browser to it after sign-in. A fetch follows it cross-origin too,
// meets a page with no CORS header, and the browser reports
// `TypeError: Failed to fetch` — no status at all. So nothing above
// saw an auth failure: the page rendered the throw as its own error,
// and the only way back was to load / by hand, which Access then
// redirected properly. That was David's report, word for word.
//
// The answer is what he did by hand, aimed at the right url: load THIS
// page again as a document, so Access re-authenticates and lands him
// on it. Not /login — the gateway session is not what lapsed — and
// never /.
// ---------------------------------------------------------------------

/** The identity probe. The gateway answers it with JSON or a 401 and
 *  never with a redirect, so a redirect on it is the edge speaking. */
export const IDENTITY_PROBE = '/api/session';

/** sessionStorage key holding when this tab last reloaded for the edge. */
export const EDGE_REAUTH_KEY = 'boss.edge-reauth-at';

/** A second edge re-authentication inside this window is refused: an
 *  edge that still redirects reads straight after a sign-in would
 *  otherwise reload the tab forever. The page's own error stands. */
export const EDGE_REAUTH_QUIET_MS = 60_000;

/** One probe per this interval at most — a real network outage fails
 *  every read, and each one must not ask the server again. */
const EDGE_PROBE_INTERVAL_MS = 30_000;

/** The slice of `window` the guard needs. */
export type GuardWindow = {
  location: { pathname: string; search: string; href: string; origin: string; reload(): void };
};

/** The slice of sessionStorage the loop guard needs. */
export type EdgeStore = { getItem(key: string): string | null; setItem(key: string, value: string): void };

/** Where the guard reads its world — injected so tests need no DOM. */
export type GuardEnv = {
  win(): GuardWindow | null;
  store(): EdgeStore | null;
  now(): number;
};

function browserEnv(): GuardEnv {
  return {
    win: () => (globalThis as { window?: GuardWindow }).window ?? null,
    store: () => {
      try {
        return (globalThis as { sessionStorage?: EdgeStore }).sessionStorage ?? null;
      } catch {
        return null;
      }
    },
    now: () => Date.now(),
  };
}

/** Reload this page as a document so the edge re-authenticates and
 *  returns here. Returns whether it issued the reload.
 *
 *  Refuses when it cannot be sure it is not looping: a reload already
 *  issued from this tab inside EDGE_REAUTH_QUIET_MS, or a tab that
 *  cannot record that it is about to reload (no storage). It shares
 *  goToLogin's once-per-page latch: a page leaves once, either way. */
export function reauthenticateAtEdge(win: GuardWindow | null, store: EdgeStore | null, now: number): boolean {
  if (!win || !store) return false;
  if (alreadyLeaving) return false;
  try {
    const last = Number(store.getItem(EDGE_REAUTH_KEY));
    if (Number.isFinite(last) && last > 0 && now - last < EDGE_REAUTH_QUIET_MS) return false;
    store.setItem(EDGE_REAUTH_KEY, String(now));
  } catch {
    return false;
  }
  alreadyLeaving = true;
  win.location.reload();
  return true;
}

let probeInFlight: Promise<boolean> | null = null;
let lastProbeAt = Number.NEGATIVE_INFINITY;

/** Whether the edge, not the network, failed the read: ask the identity
 *  probe WITHOUT following redirects. `opaqueredirect` means something
 *  in front of the gateway redirected it — Access asking for sign-in.
 *  A throw means nothing answered (an outage); an answer means the
 *  server is reachable. Concurrent failures share one probe. */
function edgeDemandsSignIn(orig: typeof fetch, now: number): Promise<boolean> {
  if (probeInFlight) return probeInFlight;
  if (now - lastProbeAt < EDGE_PROBE_INTERVAL_MS) return Promise.resolve(false);
  lastProbeAt = now;
  const probe = (async () => {
    try {
      const r = await orig(IDENTITY_PROBE, { redirect: 'manual', credentials: 'same-origin', cache: 'no-store' });
      return r.type === 'opaqueredirect';
    } catch {
      return false;
    }
  })();
  probeInFlight = probe;
  void probe.finally(() => {
    if (probeInFlight === probe) probeInFlight = null;
  });
  return probe;
}

/** The request's path when it is for this origin, else null. A string
 *  is resolved against the page; a Request's url is already absolute,
 *  which the old `startsWith('/api/')` read as another site's. */
function sameOriginPath(input: RequestInfo | URL, origin: string): string | null {
  const raw = typeof input === 'string' ? input : input instanceof URL ? input.href : input.url;
  try {
    const u = new URL(raw, origin);
    return u.origin === origin ? u.pathname : null;
  } catch {
    return null;
  }
}

/** `/api/session` and `/api/auth/*` ask "who am I"; a 401 there is the
 *  ANSWER (nobody), consumed by the session state machine. */
function isIdentityProbe(path: string): boolean {
  return path.startsWith(IDENTITY_PROBE) || path.startsWith('/api/auth/');
}

/** Wrap `fetch` so every auth failure a page's reads meet leaves the
 *  page for the door that fixes it, with the way back kept:
 *
 *  - a 401 on a same-origin /api/* read (not an identity probe): the
 *    gateway session is dead — goToLogin, ?next= = this page;
 *  - a read that THROWS (TypeError) on this origin, when the identity
 *    probe shows the edge redirecting: Access lapsed — reload this
 *    page so Access re-authenticates and returns to it.
 *
 *  A 403 is not here on purpose: the gateway answers 401 to a missing,
 *  forged or expired session, so a 403 is a live session refused, and
 *  signing in again would bring back the same role and the same 403.
 *
 *  The response, or the original throw, always reaches the caller
 *  unchanged — the guard adds a navigation, never hides a failure. */
export function guardFetch(orig: typeof fetch, env: GuardEnv = browserEnv()): typeof fetch {
  return (async (input: RequestInfo | URL, init?: RequestInit): Promise<Response> => {
    let resp: Response;
    try {
      resp = await orig(input, init);
    } catch (err) {
      const win = env.win();
      const path = win ? sameOriginPath(input, win.location.origin) : null;
      // An AbortError is the caller cancelling; only a TypeError is
      // the network layer refusing the response.
      if (path !== null && err instanceof TypeError && (await edgeDemandsSignIn(orig, env.now()))) {
        reauthenticateAtEdge(win, env.store(), env.now());
      }
      throw err;
    }
    if (resp.status === 401) {
      const win = env.win();
      const path = win ? sameOriginPath(input, win.location.origin) : null;
      if (path !== null && path.startsWith('/api/') && !isIdentityProbe(path)) goToLogin(win);
    }
    return resp;
  }) as typeof fetch;
}

export function _resetDeadSessionForTests(): void {
  alreadyLeaving = false;
  probeInFlight = null;
  lastProbeAt = Number.NEGATIVE_INFINITY;
}
