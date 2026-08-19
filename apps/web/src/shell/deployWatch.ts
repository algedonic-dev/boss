// Deploy awareness for a long-lived SPA tab (72c7c36e).
//
// A tab runs whatever bundle index.html named when it loaded, forever —
// and this system deploys at least twice a day. Measured 2026-08-19:
// the decision-context panel was verifiably in the served bundle and
// rendering correctly while David's open tab, loaded before the train
// landed, still showed the old surface. He read it as "my review flow
// is so broken"; the trap re-arms at every landing.
//
// The detection needs no server support: the built index names its
// main chunk with a content hash (`/dashboard/chunk-<hash>.js`), so
// "a deploy happened" is exactly "a fresh fetch of index.html names a
// different main chunk than the one this tab booted from". Pure
// functions here; UpdateBar owns the timer, the focus listener, and
// the fetch.

const MAIN_CHUNK = /\/dashboard\/chunk-[a-z0-9]+\.js/;

/// The main-chunk asset path an index.html names, or null when none is
/// found (the dev server serves unhashed modules — the watcher then
/// stays quiet rather than crying wolf on every focus).
export function extractMainAsset(html: string): string | null {
  const m = html.match(MAIN_CHUNK);
  return m ? m[0] : null;
}

/// True when a freshly served index names a different main chunk than
/// the one this tab booted from. Null on either side means "cannot
/// tell" and never offers a reload — a missing signal must not nag.
export function deployHasLanded(booted: string | null, served: string | null): boolean {
  return booted !== null && served !== null && booted !== served;
}
