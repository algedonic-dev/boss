import { describe, expect, test } from 'bun:test';
import { readdirSync, readFileSync } from 'node:fs';
import { join, relative } from 'node:path';
import { parseRoute } from '../router';
import { ROUTE_CATALOG } from '../shell/nav-catalog';
import { ROUTES, RETIRED_ROUTES } from '../../tests/mocked/_routes';

// THE JOBS LIST WENT WITH THE /ux BUSINESS PAGES (step 1 of design
// 01fa5bdf, David 2026-11-17: "the /ux business pages are removed; when
// unsure, remove"). The design names jobs among them; the first build of
// that car kept /ux/jobs and its "All jobs" row because search, the
// vendor page and the new-job form read through it, and review df7aeb9c
// returned it for exactly that. The list is gone; the job and step
// DETAIL pages stay, because every packet link, search hit and step
// plugin opens a packet there.
//
// Four facts, each read where it is decided: the router (not found), the
// crawl roster (retired, not crawled), the catalog (no row), and the
// source (nothing builds a link to the list, so no reader lands on
// not-found).

describe('the /ux/jobs list is retired', () => {
  test('/ux/jobs is not found, retired and uncrawled', () => {
    expect(parseRoute('/ux/jobs')).toEqual({ kind: 'notFound', path: '/ux/jobs' });
    expect(parseRoute('/ux/jobs', '?subject_id=x')).toEqual({ kind: 'notFound', path: '/ux/jobs' });
    expect(RETIRED_ROUTES).toContain('/ux/jobs');
    expect(ROUTES).not.toContain('/ux/jobs');
  });

  test('no catalog row is the list, under its path or its label', () => {
    const rows = Object.values(ROUTE_CATALOG);
    expect(rows.filter((r) => r.path === '/ux/jobs' || r.label === 'All jobs')).toEqual([]);
  });

  test('the job and step detail pages stay', () => {
    expect(parseRoute('/ux/jobs/job-1')).toEqual({ kind: 'jobDetail', jobId: 'job-1' });
    expect(parseRoute('/ux/jobs/job-1/steps/step-1')).toMatchObject({
      kind: 'stepFocus',
      jobId: 'job-1',
      stepId: 'step-1',
    });
  });
});

// The readers: a link built to /ux/jobs?… (a subject's packets, a search
// hit's "its work", the vendor page's new-job form) or to bare /ux/jobs
// would now land on not-found. Comments are stripped first, since prose
// naming the old page is history, not a link.
const ROOTS = [join(import.meta.dir, '..'), join(import.meta.dir, '../../../../libs/web-kit/src')];

function sources(dir: string): ReadonlyArray<string> {
  return readdirSync(dir, { withFileTypes: true }).flatMap((entry) => {
    const path = join(dir, entry.name);
    if (entry.isDirectory()) return entry.name === 'node_modules' ? [] : sources(path);
    const code = /\.(ts|svelte)$/.test(entry.name) && !/\.test\.ts$/.test(entry.name);
    return code ? [path] : [];
  });
}

function withoutComments(text: string): string {
  return text
    .replace(/<!--[\s\S]*?-->/g, '')
    .replace(/\/\*[\s\S]*?\*\//g, '')
    .replace(/(^|[^:])\/\/.*$/gm, '$1');
}

/** The list's path as a link target: followed by a query, or closing a
 *  string. `/ux/jobs/` (a packet or a step) is not it. */
const LIST_LINK = /\/ux\/jobs(?:\?|`|'|")/;

describe('nothing links to the retired list', () => {
  const files = ROOTS.flatMap((root) =>
    sources(root).map((path) => ({ name: relative(join(import.meta.dir, '../../../..'), path), text: readFileSync(path, 'utf8') })),
  );

  test('the scan reads both trees, and the pattern tells the list from a packet', () => {
    const names = files.map((f) => f.name);
    expect(names).toContain(join('apps', 'web', 'src', 'jobs', 'types.ts'));
    expect(names).toContain(join('libs', 'web-kit', 'src', 'GlobalSearch.svelte'));
    expect(LIST_LINK.test('href(`/ux/jobs?subject_id=${id}`)')).toBe(true);
    expect(LIST_LINK.test("path: '/ux/jobs',")).toBe(true);
    expect(LIST_LINK.test('href(`/ux/jobs/${id}`)')).toBe(false);
    expect(LIST_LINK.test(withoutComments('// the /ux/jobs? filter'))).toBe(false);
  });

  test('no source file builds a link to /ux/jobs', () => {
    const offenders = files.filter((f) => LIST_LINK.test(withoutComments(f.text))).map((f) => f.name);
    expect(offenders).toEqual([]);
  });
});
