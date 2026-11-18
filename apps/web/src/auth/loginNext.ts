// Where sign-in returns to — the `?next=` a dead session put on /login
// (web-kit session/deadSession.ts, or the gateway's login_redirect).
//
// The `next` param is attacker-reachable (anything can link to
// /login?next=...), so it admits in-app paths only — the same rule
// router.ts applies to `from`, where the comment names the attack: a
// value on another origin turns sign-in into an open redirect.
// `//evil.example` is scheme-relative and just as hostile as
// `https://`, and so are `/\evil.example` and a `next` carrying a tab
// or newline, which a browser strips to `//`; safeSitePath refuses all
// of them (4f1f7698). This file is the ONE place LoginPage reads it.
//
// Backlog 7d70cad6 (David, 2026-11-16): every door off this page must
// carry it. The passkey button — the IdP round trip David signs in
// with — went to the gateway bare, and the callback landed on `/`, so a
// breadcrumb that reached /login intact was dropped one click later.

import { safeSitePath } from '@boss/web-kit/links';

/** The in-app page to land on after sign-in, read off /login's own
 *  query; `/` when there is none or it is not an in-app path. */
export function safeNext(search: string): string {
  return safeSitePath(new URLSearchParams(search).get('next')) ?? '/';
}

/** The gateway's IdP door, carrying the page to come back to. The
 *  gateway keeps it across the round trip and returns through
 *  `/login?next=`, where `safeNext` judges it again — so what is sent
 *  here is only ever a value this function already admitted. (The path
 *  is written out, not held in a const: the gateway's route pin reads
 *  `const X = '/api/…'` as an API base and probes beneath it.) */
export function oidcLoginHref(search: string): string {
  const next = safeNext(search);
  return next === '/'
    ? '/api/auth/oidc/login'
    : `/api/auth/oidc/login?next=${encodeURIComponent(next)}`;
}
