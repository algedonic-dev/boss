// The stale-tab detector's whole contract (72c7c36e): recognize the
// built index's hashed main chunk, notice when a fresh fetch names a
// different one, and stay silent when either side is unreadable.

import { describe, expect, test } from 'bun:test';
import { deployHasLanded, extractMainAsset } from './deployWatch';

// The served index's real shape, captured from the cluster front on
// 2026-08-19 — the day the trap cost a review session.
const INDEX = `<link rel="stylesheet" crossorigin href="/dashboard/chunk-xt9sfnew.css">` +
  `<script type="module" crossorigin src="/dashboard/chunk-b9433gxg.js"></script>`;

describe('deploy watch', () => {
  test('reads the hashed main chunk out of a built index', () => {
    expect(extractMainAsset(INDEX)).toBe('/dashboard/chunk-b9433gxg.js');
  });

  test('an unhashed dev index yields null and therefore silence', () => {
    expect(extractMainAsset('<script type="module" src="/src/main.ts"></script>')).toBeNull();
    expect(deployHasLanded(null, '/dashboard/chunk-abc.js')).toBe(false);
    expect(deployHasLanded('/dashboard/chunk-abc.js', null)).toBe(false);
  });

  test('a deploy is exactly a changed main chunk', () => {
    expect(deployHasLanded('/dashboard/chunk-b9433gxg.js', '/dashboard/chunk-b9433gxg.js')).toBe(false);
    expect(deployHasLanded('/dashboard/chunk-b9433gxg.js', '/dashboard/chunk-z1y2x3w4.js')).toBe(true);
  });
});
