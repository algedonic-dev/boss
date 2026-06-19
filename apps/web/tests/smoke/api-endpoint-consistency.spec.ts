// Lint test: every literal `/api/...` URL the SPA fetches must
// either return a non-404 status, or be in the known-optional
// skip list with a documented reason.
//
// Why this exists: on 2026-05-22 the user hit two 404s by clicking
// around the SPA — `/api/people/workflows` (HrPage tab; endpoint
// not implemented) and `/api/subject-kinds/active` (JobKindNewPage;
// wrong URL, real one is `/api/subject-kinds`). Both were silent
// from the user's perspective (the SPA swallowed the failure and
// rendered an empty state), but generated 404s in the network tab
// that a careful auditor noticed. No existing test caught either:
// `readme-entry-urls.spec.ts` only flags 5xx, and
// `_console-crawl.spec.ts` (a) is opt-in, (b) only visits page
// roots, not the tabs / lazy-loads that fire follow-up fetches.
//
// This spec inventories every `fetch('/api/<literal>')` call site
// in src/ and probes each. Templated paths (`/api/jobs/${id}`,
// `/api/people/${empId}`) are skipped — they need real ids; their
// existence is implicit in the detail-page tests.

import { test, expect } from '@playwright/test';
import { readdirSync, readFileSync, statSync } from 'node:fs';
import { join } from 'node:path';

const SRC_DIR = join(import.meta.dir, '..', '..', 'src');

// Endpoints that legitimately return non-200 in this deployment.
// Each entry needs a one-line reason — a future maintainer should
// be able to delete an entry by either fixing the endpoint or
// reading the reason and deciding the skip is still warranted.
const SKIP_LIST: ReadonlyArray<{ url: string; reason: string }> = [
  // file-attachments returns 200 + `{kind: "unconfigured"}` envelope
  // when no [files] config block is set. The presigned-upload routes
  // below are server-only (no current SPA caller on the local-disk
  // backend) and POST-only, so a GET probe 4xx's. Skip them.
  { url: '/api/files/_finalize', reason: 'POST-only; GET probe always 405/404' },
  { url: '/api/files/_upload-url', reason: 'POST-only; GET probe always 405/404' },
  // `/api/auth/*` are POST endpoints; GET probe returns 405.
  { url: '/api/auth/login', reason: 'POST-only' },
  { url: '/api/auth/logout', reason: 'POST-only' },
  { url: '/api/auth/onboard', reason: 'POST-only' },
  { url: '/api/auth/issue-reset', reason: 'POST-only' },
  { url: '/api/auth/reset', reason: 'POST-only' },
  // gateway perf reset is POST-only.
  { url: '/api/gateway/perf/reset', reason: 'POST-only' },
  // messages send is POST-only.
  { url: '/api/messages/send', reason: 'POST-only' },
  // /api/people/workflows — cross-employee aggregation endpoint is
  // not yet implemented server-side; SPA renders an honest callout.
  // Skip until the endpoint lands (see boss-people TODO).
  { url: '/api/people/workflows', reason: 'not yet implemented; SPA renders an honest "not wired" callout' },
  // /api/commerce/invoices/create — POST-only.
  { url: '/api/commerce/invoices/create', reason: 'POST-only' },
  // /api/inventory/orders/create — POST-only.
  { url: '/api/inventory/orders/create', reason: 'POST-only' },
  // /api/jobs/sim-clock/pause/resume/restart-epoch — POST-only.
  { url: '/api/jobs/sim-clock/pause', reason: 'POST-only' },
  { url: '/api/jobs/sim-clock/resume', reason: 'POST-only' },
  { url: '/api/jobs/sim-clock/restart-epoch', reason: 'POST-only' },
  // /api/jobs/sim-clock/stream — Server-Sent Events; GET works but
  // hangs; skip to keep the test fast.
  { url: '/api/jobs/sim-clock/stream', reason: 'SSE stream; GET hangs' },
];

const SKIP_URLS = new Set(SKIP_LIST.map((s) => s.url));

/// Recursively walk a directory and collect file paths matching
/// the extensions we want.
function walk(dir: string, exts: ReadonlyArray<string>): string[] {
  const out: string[] = [];
  for (const entry of readdirSync(dir)) {
    const full = join(dir, entry);
    const stat = statSync(full);
    if (stat.isDirectory()) {
      out.push(...walk(full, exts));
    } else if (exts.some((e) => entry.endsWith(e))) {
      out.push(full);
    }
  }
  return out;
}

/// Extract literal `/api/...` URLs from `fetch(...)` and
/// `fetchPaged(...)` calls. Skips templated URLs (anything
/// containing `${` or `}`).
function extractEndpoints(text: string): Set<string> {
  const out = new Set<string>();
  // Match fetch('/api/...') and fetch("/api/...") with optional
  // query string. Stop at the closing quote.
  const re = /(?:fetch|fetchPaged)(?:<[^>]+>)?\(\s*['"]([^'"`$]*\/api\/[^'"`$?]+)(?:\?[^'"`$]*)?['"]/g;
  let m: RegExpExecArray | null;
  while ((m = re.exec(text)) !== null) {
    const url = m[1]!;
    // Skip anything with template-literal markers — those have
    // runtime-substituted ids.
    if (url.includes('${') || url.includes('}')) continue;
    out.add(url);
  }
  return out;
}

test('every literal /api URL the SPA fetches resolves to a non-404', async ({ request }) => {
  test.setTimeout(120_000);

  // Inventory every fetch URL.
  const files = walk(SRC_DIR, ['.svelte', '.ts']);
  const allUrls = new Set<string>();
  for (const f of files) {
    const text = readFileSync(f, 'utf8');
    for (const url of extractEndpoints(text)) {
      allUrls.add(url);
    }
  }

  // Probe each (skipping the known-optional list).
  const failures: { url: string; status: number }[] = [];
  const checked: string[] = [];
  for (const url of [...allUrls].sort()) {
    if (SKIP_URLS.has(url)) continue;
    const r = await request.get(url, { failOnStatusCode: false });
    checked.push(url);
    if (r.status() === 404) {
      failures.push({ url, status: r.status() });
    }
  }

  // Surface inventory size so a regression that DROPS endpoints
  // (i.e. the extractor stops matching) shows up obviously.
  console.log(
    `\n=== api-endpoint-consistency: inventoried ${allUrls.size} unique URLs, ` +
      `probed ${checked.length} (${SKIP_URLS.size} skipped) ===\n`,
  );
  if (failures.length > 0) {
    console.log('FAIL:');
    for (const f of failures) {
      console.log(`  ${f.status}  ${f.url}`);
    }
  }

  expect(
    failures,
    `SPA calls ${failures.length} /api endpoint(s) that return 404. Either ` +
      `(a) fix the URL in the SPA, (b) implement the endpoint server-side, or ` +
      `(c) add it to SKIP_LIST in this spec with a one-line reason.\n` +
      failures.map((f) => `  ${f.status} ${f.url}`).join('\n'),
  ).toEqual([]);
});
