import { describe, expect, test } from 'bun:test';
import { readdirSync, readFileSync } from 'node:fs';
import { join, relative } from 'node:path';

/** An empty list says WHICH empty it is, through one helper (backlog
 *  0ef5e008, 2026-09-29).
 *
 *  Seven list pages wrote their own "No X match those filters." as the
 *  only empty branch, so an empty source read as the operator's filters
 *  hiding it; three more had each grown the missing branch by hand.
 *  `emptyState` / `emptyLine` in readState.ts answer it once — read
 *  failed, nothing exists, filters hid it — and ListEmpty.svelte renders
 *  the answer. These pins hold the tree to that: no Svelte file writes the
 *  filters line itself, and each page the packet named renders ListEmpty.
 *  A page that writes the line again has an empty branch the helper does
 *  not decide, which is the defect this pins out. */

const SRC = join(import.meta.dir, '..');

function svelteFiles(dir: string): ReadonlyArray<string> {
  return readdirSync(dir, { withFileTypes: true }).flatMap((entry) => {
    const path = join(dir, entry.name);
    if (entry.isDirectory()) return svelteFiles(path);
    return entry.name.endsWith('.svelte') ? [path] : [];
  });
}

const FILES: ReadonlyArray<Readonly<{ name: string; text: string }>> = svelteFiles(SRC).map((path) => ({
  name: relative(SRC, path),
  text: readFileSync(path, 'utf8'),
}));

/** The filters line, as a page would type it (case-insensitive; a label
 *  default in getLabel('…', 'No X match those filters.') is a copy too). */
const OWN_FILTERS_LINE = /match(?:es)? those filters/i;

/** The pages backlog 0ef5e008 named (WatchlistPage was already fixed and
 *  joined anyway), plus the three that had hand-grown the branch. */
const LIST_PAGES: ReadonlyArray<string> = [
  join('accounts', 'AccountsList.svelte'),
  join('people', 'PeopleList.svelte'),
  join('parts', 'PartsList.svelte'),
  join('finance', 'InvoicesTab.svelte'),
  join('shipping', 'ShippingPage.svelte'),
  join('inbox', 'InboxPage.svelte'),
  join('catalog', 'CatalogBrowser.svelte'),
  join('accounts', 'WatchlistPage.svelte'),
  join('vendors', 'VendorsList.svelte'),
  join('marketing-assets', 'MarketingAssetsList.svelte'),
];

describe('an empty list says why, through one helper', () => {
  test('the pin reads the tree it pins', () => {
    // Control: the walk reaches every page named, and the pattern
    // catches the line in the shapes it was written in.
    const names = FILES.map((f) => f.name);
    for (const page of LIST_PAGES) expect(names).toContain(page);
    expect(OWN_FILTERS_LINE.test('<p class="empty">No parts match those filters.</p>')).toBe(true);
    expect(OWN_FILTERS_LINE.test("getLabel('catalog.empty_state', 'No devices match those filters.')")).toBe(true);
    expect(OWN_FILTERS_LINE.test('<p class="empty">No parts yet.</p>')).toBe(false);
  });

  test('no Svelte file writes its own filters line', () => {
    const offenders = FILES.filter((f) => OWN_FILTERS_LINE.test(f.text)).map((f) => f.name);
    expect(offenders).toEqual([]);
  });

  test('every named list page renders ListEmpty', () => {
    const missing = LIST_PAGES.filter((page) => {
      const text = FILES.find((f) => f.name === page)?.text ?? '';
      return !/<ListEmpty\b/.test(text);
    });
    expect(missing).toEqual([]);
  });
});
