import { describe, expect, test } from 'bun:test';

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

/** The jobs-list URL every offending line contains, as a fixed string. */
const JOBS_LIST = '/api/jobs?';

/** A jobs-list URL with a literal limit of 200 or more (3+ digits, the
 *  first making it >= 200 or 4+ digits). */
const BIG_LITERAL_LIMIT = /\/api\/jobs\?[^'"`\s]*\blimit=(?:[2-9]\d\d|\d{4,})\b/;

/** A comment line naming the old shape is history, not a read. */
const COMMENT = /^\s*(\/\/|\*|\/\*)/;

function offending(line: string): boolean {
  return BIG_LITERAL_LIMIT.test(line) && !COMMENT.test(line) && !line.includes('fetchPaged');
}

function offenders(name: string, text: string): ReadonlyArray<string> {
  return text.split('\n').flatMap((line, i) => (offending(line) ? [`${name}:${i + 1}: ${line.trim()}`] : []));
}

/** Every line of src/ that names the jobs list, read ONCE, by git, at
 *  module load (backlog a653fe65, 2026-10-09).
 *
 *  This file walked src/ in JS and read all 362 .ts/.svelte files inside
 *  the test, one at a time. 43 ms on a quiet dev pod; 139 057 ms in train gate 09d635a3
 *  (train a3ed3485), past the runner's 120 s budget — and the sister pin
 *  an-empty-list-says-why.test.ts, reading 167 .svelte files the same
 *  way, took 86 531 ms in the same run. A loaded gate reads file
 *  contents hundreds of times slower than a quiet pod, while the walk
 *  itself (the first test) still took 6.59 ms there. Two changes, and
 *  neither narrows what is pinned:
 *
 *  - `git grep -F` reads the tree natively, on several threads at once
 *    rather than one file after another, and hands back only the ~60
 *    lines that name `/api/jobs?` (57 ms quiet — no faster than the
 *    walk there; the threads are for the loaded gate) — every line
 *    BIG_LITERAL_LIMIT can match contains that fixed string, so the
 *    prefilter cannot drop an offender, and `offending` still judges
 *    each line. `--untracked` keeps a new, not-yet-added file in view,
 *    as the walk had it; src/ holds no ignored files.
 *  - It runs here rather than in a test, as the sister pin's read does.
 *    The read is synchronous, so a per-test timeout could never have
 *    interrupted it: the 120 s budget only turned a slow read into a red
 *    after it finished. The file's wall time still lands on the gate's
 *    receipt (`web_timings`), so a slow read stays visible. */
const SEARCH = Bun.spawnSync(
  [
    'git', 'grep', '-z', '-n', '--full-name', '-I', '--untracked', '--no-color', '-F', '-e', JOBS_LIST,
    '--', '*.ts', '*.svelte', ':!*.test.ts',
  ],
  { cwd: import.meta.dir + '/..', stdout: 'pipe', stderr: 'pipe' },
);

/** `path\0line\0text` per line (-z), path relative to the repo root. */
const CANDIDATES: ReadonlyArray<Readonly<{ name: string; line: number; text: string }>> = SEARCH.stdout
  .toString()
  .split('\n')
  .filter((row) => row !== '')
  .map((row) => {
    const [name = '', line = '0', ...text] = row.split('\0');
    return { name, line: Number(line), text: text.join('\0') };
  });

describe('a whole read of the jobs list pages to its total', () => {
  test('the pin reads the tree it pins, and catches the shape it was filed on', () => {
    // git grep answers 0 with matches and 1 with none; anything else (no
    // repo, no git) is a search that did not happen, and says why.
    expect({ status: SEARCH.exitCode, stderr: SEARCH.stderr.toString() }).toEqual({ status: 0, stderr: '' });
    // Control: the search reached .svelte and .ts files that read the
    // jobs list (each reads it through fetchEvery since 2026-09-27) …
    const names = CANDIDATES.map((c) => c.name);
    expect(names).toContain('apps/web/src/jobs/TriageFlow.svelte');
    expect(names).toContain('apps/web/src/it/yard/yard.ts');
    // … and left the test files out: this one names the shape in its
    // planted lines below, so a search that read it would red the pin.
    expect(names.filter((n) => n.endsWith('.test.ts'))).toEqual([]);
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
    const found = CANDIDATES.filter((c) => offending(c.text)).map((c) => `${c.name}:${c.line}: ${c.text.trim()}`);
    expect(found).toEqual([]);
  });
});
