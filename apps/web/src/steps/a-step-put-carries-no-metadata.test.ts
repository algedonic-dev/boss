import { describe, expect, test } from 'bun:test';
import { readdirSync, readFileSync } from 'node:fs';
import { join, relative } from 'node:path';

/** Every step write in the web leaves through ONE door file (backlog
 *  e39a9d2a, design 93d2bddb, Stage 2 car 2).
 *
 *  The step PUT replaces `metadata` wholesale, and the decided end state
 *  is that it refuses ANY metadata body: metadata goes to the step merge
 *  door (`PATCH …/steps/{id}/metadata`), the status alone to the PUT.
 *  Nine web surfaces each built their own `fetch(…/steps/{id}, {method:
 *  'PUT', body: {…step.metadata, key}})` — a read-merge-write that drops
 *  anything written to the step since the page read it — and each had to
 *  be found by hand, because nothing said where a step write may live.
 *
 *  Now one file builds a step's own URL: `@boss/web-kit/step-doors`,
 *  whose `saveStep` sends metadata through the merge door and whose
 *  `putStep` refuses a body carrying metadata. This pin holds the rest of
 *  the web to it: a new surface that writes a step by hand fails here,
 *  named, instead of in production when the tighten lands. The URL is the
 *  thing pinned, not the method, because a step's own resource is only
 *  ever written (reads go through the job or the `/steps` list), and a
 *  hand-built URL is where every one of the nine started. */

const WEB_SRC = import.meta.dir.replace(/[/\\]steps$/, '');
const KIT_SRC = join(WEB_SRC, '..', '..', '..', 'libs', 'web-kit', 'src');
const ROOT = join(WEB_SRC, '..', '..', '..');

/** The one file allowed to spell a step's own URL. */
const DOOR = join('libs', 'web-kit', 'src', 'step-doors.ts');

function sources(dir: string): ReadonlyArray<string> {
  return readdirSync(dir, { withFileTypes: true }).flatMap((entry) => {
    const path = join(dir, entry.name);
    if (entry.isDirectory()) return entry.name === 'node_modules' ? [] : sources(path);
    const src = /\.(ts|svelte)$/.test(entry.name) && !entry.name.endsWith('.test.ts');
    return src ? [path] : [];
  });
}

const FILES: ReadonlyArray<Readonly<{ name: string; text: string }>> = [
  ...sources(WEB_SRC),
  ...sources(KIT_SRC),
].map((path) => ({ name: relative(ROOT, path), text: readFileSync(path, 'utf8') }));

/** A template literal that ENDS at a step id under /api/jobs — the step's
 *  own resource, not its `/metadata`, `/claim` or `/sign-offs` doors. */
const STEP_RESOURCE_URL = /`\/api\/jobs\/[^`]*\/steps\/\$\{[^}]*\}`/g;

function hits(text: string): ReadonlyArray<number> {
  return [...text.matchAll(STEP_RESOURCE_URL)].map(
    (m) => text.slice(0, m.index).split('\n').length,
  );
}

describe('a step PUT carries no metadata — one door builds the step URL', () => {
  test('the pin reads the tree it pins', () => {
    // Control: the walk reaches both trees, and the pattern matches the
    // door's own spelling while passing over the doors beside it.
    const names = FILES.map((f) => f.name);
    expect(names).toContain(DOOR);
    expect(names).toContain(join('apps', 'web', 'src', 'jobs', 'TriageBoard.svelte'));
    expect(names).toContain(join('libs', 'web-kit', 'src', 'FeedbackControl.svelte'));
    expect(hits(FILES.find((f) => f.name === DOOR)?.text ?? '').length).toBeGreaterThan(0);
    expect(hits('fetch(`/api/jobs/${encodeURIComponent(j)}/steps/${encodeURIComponent(s)}`, {')).toEqual([1]);
    expect(hits('`/api/jobs/${j}/steps/${s}/metadata`')).toEqual([]);
    expect(hits('`/api/jobs/${j}/steps/${s}/claim${query}`')).toEqual([]);
    expect(hits('`/ux/jobs/${j}/steps/${s}`')).toEqual([]);
  });

  test('no file but the door builds a step resource URL', () => {
    const offenders = FILES.filter((f) => f.name !== DOOR).flatMap((f) =>
      hits(f.text).map((line) => `${f.name}:${line}`),
    );
    expect(offenders).toEqual([]);
  });
});
