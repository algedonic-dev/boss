import { describe, expect, test } from 'bun:test';
import { readdirSync, readFileSync } from 'node:fs';
import { join, relative } from 'node:path';

/** A read of the jobs list that wants the whole set pages to its total
 *  (backlog b68a9dde, 2026-09-27).
 *
 *  Seven pages read `/api/jobs?…&limit=200` once and kept the page with
 *  no look at `total`. Two lost rows that day — the yard and the
 *  conductor's feed read ship-a-change (978 packets) and the triage flow
 *  read open backlog-items (361) — and five more were waiting for their
 *  kind to grow. Each now reads through `fetchEvery` (paginated.ts),
 *  which pages until the rows it holds reach the total or says it
 *  stopped.
 *
 *  This pin refuses the shape they shared: a `/api/jobs?` URL with a
 *  LITERAL `limit=` of 200 or more. A limit that big is a read that
 *  wants everything, and everything is `fetchEvery`'s. What it leaves
 *  alone is deliberate: a small newest-N window (`limit=40` trains,
 *  `limit=1` for a count) is a window, not a whole read, and a line
 *  calling `fetchPaged` holds the page's `total` in its answer. */

const SRC = join(import.meta.dir, '..');

function sourceFiles(dir: string): ReadonlyArray<string> {
  return readdirSync(dir, { withFileTypes: true }).flatMap((entry) => {
    const path = join(dir, entry.name);
    if (entry.isDirectory()) return sourceFiles(path);
    if (entry.name.endsWith('.test.ts')) return [];
    return entry.name.endsWith('.svelte') || entry.name.endsWith('.ts') ? [path] : [];
  });
}

/** A jobs-list URL with a literal limit of 200 or more (3+ digits, the
 *  first making it >= 200 or 4+ digits). */
const BIG_LITERAL_LIMIT = /\/api\/jobs\?[^'"`\s]*\blimit=(?:[2-9]\d\d|\d{4,})\b/;

/** A comment line naming the old shape is history, not a read. */
const COMMENT = /^\s*(\/\/|\*|\/\*)/;

function offenders(name: string, text: string): ReadonlyArray<string> {
  return text
    .split('\n')
    .flatMap((line, i) =>
      BIG_LITERAL_LIMIT.test(line) && !COMMENT.test(line) && !line.includes('fetchPaged')
        ? [`${name}:${i + 1}: ${line.trim()}`]
        : [],
    );
}

describe('a whole read of the jobs list pages to its total', () => {
  test('the pin reads the tree it pins, and catches the shape it was filed on', () => {
    const names = sourceFiles(SRC).map((p) => relative(SRC, p));
    expect(names).toContain(join('jobs', 'TriageFlow.svelte'));
    expect(names).toContain(join('it', 'yard', 'yard.ts'));
    // Control: each of the seven shapes of 2026-09-27 is caught …
    for (const line of [
      "fetch('/api/jobs?kind=ship-a-change&limit=200'),",
      'fetch(`/api/jobs?kind=${encodeURIComponent(kind)}&status=open&limit=200`),',
      "fetch('/api/jobs?kind=pr-train&closed_within=0&limit=500')",
      "fetch('/api/jobs?kind=x&limit=1000&offset=0')",
    ]) {
      expect(offenders('planted', line)).toHaveLength(1);
    }
    // … and a window, a count and a paged-with-total read are not.
    for (const line of [
      "fetch('/api/jobs?kind=pr-train&limit=40'),",
      'fetch(`/api/jobs?kind=${encodeURIComponent(kind)}&limit=1`);',
      "fetchPaged<Job>('/api/jobs?department=support&limit=5000'),",
      "fetchEvery<Job>('/api/jobs?kind=x&status=open'),",
      '  // it read `/api/jobs?kind=x&limit=200` until 2026-09-27',
    ]) {
      expect(offenders('planted', line)).toHaveLength(0);
    }
  });

  test('no source file reads the jobs list with a big literal limit', () => {
    const found = sourceFiles(SRC).flatMap((p) => offenders(relative(SRC, p), readFileSync(p, 'utf8')));
    expect(found).toEqual([]);
  });
});
