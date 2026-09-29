import { describe, expect, test } from 'bun:test';
import { readdirSync, readFileSync } from 'node:fs';

/** A step plugin's step PUT carries the status and nothing else (backlog
 *  e39a9d2a, design 93d2bddb, Stage 2 car 3).
 *
 *  The step PUT replaces `metadata` wholesale, and the decided end state
 *  is that it refuses ANY metadata body: metadata goes to the step merge
 *  door (`PATCH …/steps/{id}/metadata`), the status alone to the PUT.
 *  The web's own surfaces are held to that by the one door file
 *  `@boss/web-kit/step-doors` (car 2, `a-step-put-carries-no-metadata`).
 *  The bundles under `infra/step-plugins/` cannot import it: each is a
 *  standalone IIFE the gateway serves as-is, with no build step, and the
 *  host loads exactly one bundle per step kind, so there is no place a
 *  shared helper could live. So each bundle spells its own two fetches,
 *  and this pin reads every one of them: the body of a fetch to a step's
 *  own resource must be `JSON.stringify({ status })` or `JSON.stringify({
 *  status: '<word>' })`, nothing more. Eleven bundles once PUT metadata
 *  there — five the whole drawn step with a snapshot spread, five a
 *  freshly re-read row, one a re-read row with no keys of its own — and
 *  every one had to be found by hand. The README is read too, because
 *  its worked example is where a new bundle copies its save from. */

const DIR = new URL('../../../../infra/step-plugins/', import.meta.url);

const FILES: ReadonlyArray<Readonly<{ name: string; text: string }>> = readdirSync(DIR)
  .filter((f) => f.endsWith('.js') || f.endsWith('.md'))
  .sort()
  .map((name) => ({ name, text: readFileSync(new URL(name, DIR), 'utf8') }));

/** A template literal that ENDS at a step id under /api/jobs — the step's
 *  own resource, not its `/metadata`, `/claim` or `/sign-offs` doors. */
const STEP_RESOURCE_URL = /`\/api\/jobs\/[^`]*\/steps\/\$\{[^}]*\}`/g;

/** The only body a step PUT may carry: the status, alone. */
const STATUS_ONLY_BODY = /^body:\s*JSON\.stringify\(\{\s*status(?:\s*:\s*'[a-z]+')?\s*\}\)/;

/** Each write to a step's own resource whose body is not the status
 *  alone, as `line: body`. The body is the first `body:` after the URL,
 *  before the next fetch — a write with no body there is an offender too,
 *  because a PUT built by spreading a prepared object hides its body. */
function offenders(text: string): ReadonlyArray<string> {
  return [...text.matchAll(STEP_RESOURCE_URL)].flatMap((m) => {
    const line = text.slice(0, m.index).split('\n').length;
    const after = text.slice((m.index ?? 0) + m[0].length);
    const next = after.search(/\bfetch\(/);
    const scope = next < 0 ? after : after.slice(0, next);
    const at = scope.search(/\bbody:/);
    const body = at < 0 ? '' : scope.slice(at);
    return STATUS_ONLY_BODY.test(body)
      ? []
      : [`${line}: ${(body.split('\n')[0] || '(no body before the next fetch)').trim()}`];
  });
}

function count(text: string): number {
  return [...text.matchAll(STEP_RESOURCE_URL)].length;
}

describe('a step plugin PUT carries no metadata — the status alone', () => {
  test('the pin reads the tree it pins, and can fail', () => {
    const names = FILES.map((f) => f.name);
    // A wrong path answers "clean" instead of erroring (CLAUDE.md §Doors).
    expect(names).toContain('sign-off.js');
    expect(names).toContain('review-design.js');
    expect(names).toContain('README.md');
    expect(FILES.reduce((n, f) => n + count(f.text), 0)).toBeGreaterThan(0);

    // Every shape the eleven bundles used, taken from their old text.
    const put = (body: string) =>
      `await fetch(\`/api/jobs/\${encodeURIComponent(jobId)}/steps/\${encodeURIComponent(step.id)}\`, {\n  method: 'PUT',\n  ${body}\n});`;
    for (const old of [
      'body: JSON.stringify(body),',
      "body: JSON.stringify({ ...fresh, job_id: jobId, status: 'completed' }),",
      "body: JSON.stringify(\n    Object.assign({}, fresh, { job_id: jobId, status: 'completed' }),\n  ),",
      'body: JSON.stringify({ ...base, job_id: jobId, status }),',
      "body: JSON.stringify({ status: 'completed', metadata: { checks } }),",
      "headers: { 'content-type': 'application/json' },",
    ]) {
      expect(offenders(put(old)).length).toBe(1);
    }
    for (const ok of [
      'body: JSON.stringify({ status }),',
      "body: JSON.stringify({ status: 'completed' }),",
    ]) {
      expect(offenders(put(ok))).toEqual([]);
    }
    // The doors beside the step's own resource are not it.
    expect(count('fetch(`/api/jobs/${jobId}/steps/${step.id}/metadata`, {')).toBe(0);
    expect(count('fetch(`/api/jobs/${jobId}/steps/${step.id}/sign-offs`, {')).toBe(0);
    expect(count("href: `/jobs/${jobId}/steps/${step.id}`,")).toBe(0);
  });

  test('every step PUT in a plugin bundle or its README carries the status alone', () => {
    const found = FILES.flatMap((f) => offenders(f.text).map((o) => `${f.name}:${o}`));
    expect(found).toEqual([]);
  });
});
