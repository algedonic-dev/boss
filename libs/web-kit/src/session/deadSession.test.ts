// A dead session goes to the login page — from ANY fetch, not only
// /api/*.
//
// The 401-redirect interceptor in App.svelte only ever watched
// /api/*, and said so. The non-API fetch that matters is a step
// plugin bundle: `/plugins/<frontend_url>` preflighted in
// pluginHost.loadPlugin. With an expired session that preflight came
// back 401 and the operator got a plugin-load error with a Retry
// button that could never succeed — a surface reporting a symptom
// while the system already held the cause.
//
// These tests pin the ONE definition both call sites now share: the
// redirect target, the ?next= encoding, the on-/login guard, and the
// once-per-page latch that keeps a dead session from restating the
// navigation for every concurrent 401.

import { afterEach, describe, expect, test } from 'bun:test';
import {
  EDGE_REAUTH_KEY,
  EDGE_REAUTH_QUIET_MS,
  LOGIN_PATH,
  _resetDeadSessionForTests,
  goToLogin,
  guardFetch,
  loginUrlFor,
  reauthenticateAtEdge,
  type GuardEnv,
} from './deadSession';

afterEach(() => {
  _resetDeadSessionForTests();
});

function fakeWindow(pathname: string, search = '') {
  return { location: { pathname, search, href: `${pathname}${search}` } };
}

describe('loginUrlFor', () => {
  test('captures path + search as an encoded ?next=', () => {
    expect(loginUrlFor('/ux/jobs', '?status=open')).toBe(
      '/login?next=%2Fux%2Fjobs%3Fstatus%3Dopen',
    );
  });

  test('a bare path needs no search', () => {
    expect(loginUrlFor('/me', '')).toBe('/login?next=%2Fme');
  });

  test('the target is the login route the router knows', () => {
    expect(LOGIN_PATH).toBe('/login');
    expect(loginUrlFor('/me', '').startsWith(`${LOGIN_PATH}?`)).toBe(true);
  });
});

describe('goToLogin', () => {
  test('navigates to the login page with the current page as ?next=', () => {
    const win = fakeWindow('/jobs/abc', '?tab=steps');
    expect(goToLogin(win)).toBe(true);
    expect(win.location.href).toBe('/login?next=%2Fjobs%2Fabc%3Ftab%3Dsteps');
  });

  test('does NOT redirect when already on /login — that is the loop', () => {
    const win = fakeWindow('/login', '?next=%2Fme');
    expect(goToLogin(win)).toBe(false);
    expect(win.location.href).toBe('/login?next=%2Fme');
  });

  test('redirects once per page: a storm of 401s issues one navigation', () => {
    const win = fakeWindow('/jobs/abc', '');
    expect(goToLogin(win)).toBe(true);
    const after = win.location.href;
    expect(goToLogin(win)).toBe(false);
    expect(goToLogin(win)).toBe(false);
    expect(win.location.href).toBe(after);
  });

  test('with no window to navigate it reports that it did not', () => {
    expect(goToLogin(null)).toBe(false);
  });
});

// ---------------------------------------------------------------------
// The fetch guard — every auth failure a page's reads can meet, decided
// in one place (backlog 7d70cad6, David 2026-11-16: "if auth fails on
// any page, I get redirected with a breadcrumb back instead of getting
// an error and needing to return to the root page").
//
// The failure David meets is Cloudflare Access expiring at the edge.
// Access answers an expired read with a 302 to its own login origin;
// fetch follows it cross-origin, the login page carries no CORS
// header, and the browser reports `TypeError: Failed to fetch` — no
// status, no 401, so the old interceptor never saw it and the page
// rendered the throw as its error. The answer is a full-page load of
// the SAME url: Access redirects a document navigation to its login
// with that url as `redirect_url`, and sends the browser back to it.
// ---------------------------------------------------------------------

type FakeGuardWindow = {
  location: {
    pathname: string;
    search: string;
    href: string;
    origin: string;
    reload: () => void;
  };
  reloads: number;
};

const ORIGIN = 'https://boss.example';

function guardWindow(pathname: string, search = ''): FakeGuardWindow {
  const win: FakeGuardWindow = {
    location: {
      pathname,
      search,
      href: `${ORIGIN}${pathname}${search}`,
      origin: ORIGIN,
      reload: () => {
        win.reloads += 1;
      },
    },
    reloads: 0,
  };
  return win;
}

function memoryStore(): { getItem(k: string): string | null; setItem(k: string, v: string): void; data: Map<string, string> } {
  const data = new Map<string, string>();
  return {
    data,
    getItem: (k) => data.get(k) ?? null,
    setItem: (k, v) => {
      data.set(k, v);
    },
  };
}

function envFor(
  win: FakeGuardWindow | null,
  store: GuardEnv['store'] extends () => infer S ? S : never,
  now = 10_000_000,
): GuardEnv {
  return { win: () => win, store: () => store, now: () => now };
}

type Call = { url: string; redirect: RequestRedirect | undefined };

/** A backend whose every read fails the way a cross-origin redirect
 *  does, and whose identity probe (asked with `redirect: 'manual'`)
 *  answers as `probe` says: `access` = the edge redirected it, `network`
 *  = nothing answered, `ok` = the server answered. */
function failingBackend(probe: 'access' | 'network' | 'ok'): { fetch: typeof fetch; calls: Call[] } {
  const calls: Call[] = [];
  const f = (async (input: RequestInfo | URL, init?: RequestInit): Promise<Response> => {
    const url = typeof input === 'string' ? input : input instanceof URL ? input.href : input.url;
    calls.push({ url, redirect: init?.redirect });
    if (init?.redirect === 'manual') {
      if (probe === 'access') return { type: 'opaqueredirect', status: 0, ok: false } as unknown as Response;
      if (probe === 'network') throw new TypeError('Failed to fetch');
      return new Response('{}', { status: 200 });
    }
    throw new TypeError('Failed to fetch');
  }) as typeof fetch;
  return { fetch: f, calls };
}

function answering(status: number): typeof fetch {
  return (async () => new Response('x', { status })) as unknown as typeof fetch;
}

describe('guardFetch: Access expiry at the edge reloads the SAME page', () => {
  test('a read that throws because the edge redirected it reloads this url, never /login or /', async () => {
    const win = guardWindow('/ux/jobs/abc', '?tab=steps');
    const store = memoryStore();
    const backend = failingBackend('access');
    const guarded = guardFetch(backend.fetch, envFor(win, store));

    await expect(guarded('/api/jobs/abc')).rejects.toThrow('Failed to fetch');

    expect(win.reloads).toBe(1);
    // A reload keeps the url: the breadcrumb IS the page.
    expect(win.location.href).toBe(`${ORIGIN}/ux/jobs/abc?tab=steps`);
    // The probe asked the identity endpoint without following.
    expect(backend.calls.at(-1)).toEqual({ url: '/api/session', redirect: 'manual' });
    // And it wrote the loop guard before leaving.
    expect(store.data.get(EDGE_REAUTH_KEY)).toBe('10000000');
  });

  test('a network outage is not an auth failure: no reload, the error stands', async () => {
    const win = guardWindow('/ux/jobs/abc');
    const guarded = guardFetch(failingBackend('network').fetch, envFor(win, memoryStore()));
    await expect(guarded('/api/jobs/abc')).rejects.toThrow('Failed to fetch');
    expect(win.reloads).toBe(0);
  });

  test('a server that answers the probe is reachable and signed in at the edge: no reload', async () => {
    const win = guardWindow('/ux/jobs/abc');
    const guarded = guardFetch(failingBackend('ok').fetch, envFor(win, memoryStore()));
    await expect(guarded('/api/jobs/abc')).rejects.toThrow('Failed to fetch');
    expect(win.reloads).toBe(0);
  });

  test('never a loop: a second re-authentication inside the quiet window is refused', async () => {
    const win = guardWindow('/ux/jobs/abc');
    const store = memoryStore();
    const now = 50_000_000;
    // The page this tab reloaded a moment ago asked the edge already.
    store.setItem(EDGE_REAUTH_KEY, String(now - 1_000));
    const guarded = guardFetch(failingBackend('access').fetch, envFor(win, store, now));
    await expect(guarded('/api/jobs/abc')).rejects.toThrow('Failed to fetch');
    expect(win.reloads).toBe(0);
  });

  test('outside the quiet window it re-authenticates again', async () => {
    const win = guardWindow('/ux/jobs/abc');
    const store = memoryStore();
    const now = 50_000_000;
    store.setItem(EDGE_REAUTH_KEY, String(now - EDGE_REAUTH_QUIET_MS - 1));
    const guarded = guardFetch(failingBackend('access').fetch, envFor(win, store, now));
    await expect(guarded('/api/jobs/abc')).rejects.toThrow('Failed to fetch');
    expect(win.reloads).toBe(1);
  });

  test('a tab that cannot record the guard does not reload — it could not stop a loop', async () => {
    const win = guardWindow('/ux/jobs/abc');
    const store = {
      getItem: () => null,
      setItem: () => {
        throw new Error('storage disabled');
      },
    };
    const guarded = guardFetch(failingBackend('access').fetch, envFor(win, store));
    await expect(guarded('/api/jobs/abc')).rejects.toThrow('Failed to fetch');
    expect(win.reloads).toBe(0);
  });

  test('a storm of failing reads asks the edge once and reloads once', async () => {
    const win = guardWindow('/ux/jobs/abc');
    const backend = failingBackend('access');
    const guarded = guardFetch(backend.fetch, envFor(win, memoryStore()));
    const all = await Promise.allSettled([
      guarded('/api/jobs/abc'),
      guarded('/api/people'),
      guarded('/plugins/review-design.js'),
    ]);
    expect(all.every((r) => r.status === 'rejected')).toBe(true);
    expect(backend.calls.filter((c) => c.redirect === 'manual')).toHaveLength(1);
    expect(win.reloads).toBe(1);
  });

  test('an aborted read is the caller cancelling, not the edge: no probe', async () => {
    const win = guardWindow('/ux/jobs/abc');
    const calls: string[] = [];
    const aborting = (async (input: RequestInfo | URL) => {
      calls.push(String(input));
      throw new DOMException('The operation was aborted.', 'AbortError');
    }) as unknown as typeof fetch;
    const guarded = guardFetch(aborting, envFor(win, memoryStore()));
    await expect(guarded('/api/jobs/abc')).rejects.toThrow('aborted');
    expect(calls).toEqual(['/api/jobs/abc']);
    expect(win.reloads).toBe(0);
  });

  test('a failed read of ANOTHER origin says nothing about ours: no probe', async () => {
    const win = guardWindow('/ux/jobs/abc');
    const backend = failingBackend('access');
    const guarded = guardFetch(backend.fetch, envFor(win, memoryStore()));
    await expect(guarded('https://elsewhere.example/x.json')).rejects.toThrow('Failed to fetch');
    expect(backend.calls).toHaveLength(1);
    expect(win.reloads).toBe(0);
  });

  test('reauthenticateAtEdge with no window reports that it did not', () => {
    expect(reauthenticateAtEdge(null, memoryStore(), 1)).toBe(false);
  });
});

describe('guardFetch: a dead gateway session goes to sign-in with the page as next', () => {
  test('a 401 on a data read redirects with ?next=', async () => {
    const win = guardWindow('/ux/jobs/abc', '?tab=steps');
    const guarded = guardFetch(answering(401), envFor(win, memoryStore()));
    const resp = await guarded('/api/jobs/abc');
    expect(resp.status).toBe(401);
    expect(win.location.href).toBe('/login?next=%2Fux%2Fjobs%2Fabc%3Ftab%3Dsteps');
  });

  test('a Request object (absolute url) is the same read and redirects too', async () => {
    const win = guardWindow('/ux/jobs/abc');
    const guarded = guardFetch(answering(401), envFor(win, memoryStore()));
    await guarded(new Request(`${ORIGIN}/api/jobs/abc`));
    expect(win.location.href).toBe('/login?next=%2Fux%2Fjobs%2Fabc');
  });

  test('a 401 on an identity probe is the ANSWER "nobody", not an expiry', async () => {
    for (const probe of ['/api/session', '/api/auth/me']) {
      _resetDeadSessionForTests();
      const win = guardWindow('/me');
      const guarded = guardFetch(answering(401), envFor(win, memoryStore()));
      await guarded(probe);
      expect(win.location.href).toBe(`${ORIGIN}/me`);
    }
  });

  test('a 403 is a refusal of a live session, not an auth failure: no redirect', async () => {
    // Signing in again yields the same role and the same 403, so a
    // redirect here would be a loop through /login.
    const win = guardWindow('/ux/jobs/abc');
    const guarded = guardFetch(answering(403), envFor(win, memoryStore()));
    const resp = await guarded('/api/jobs/abc', { method: 'POST' });
    expect(resp.status).toBe(403);
    expect(win.location.href).toBe(`${ORIGIN}/ux/jobs/abc`);
  });

  test('on /login itself a 401 never redirects — no loop', async () => {
    const win = guardWindow('/login', '?next=%2Fme');
    const guarded = guardFetch(answering(401), envFor(win, memoryStore()));
    await guarded('/api/people');
    expect(win.location.href).toBe(`${ORIGIN}/login?next=%2Fme`);
  });
});

