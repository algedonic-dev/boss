// The board is shared, and the pressure on a shared component is
// always to grow one small exception for its first consumer. This
// pins the seam: whatever `TriageBoard` renders, it does not know
// which queue it is looking at.
//
// It is a source-level assertion because that is where the coupling
// would appear. The Playwright suite proves the board *works* for
// feedback; nothing there would fail if someone reached into a
// feedback-shaped field to make one card look nicer, and that is
// exactly the change that would make the second queue need its own
// board again.

import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';

const source = readFileSync(new URL('./TriageBoard.svelte', import.meta.url), 'utf8');
const forkSource = readFileSync(new URL('./fork.ts', import.meta.url), 'utf8');

/// Comments deliberately discuss feedback — it is where the component
/// came from and why the seam exists. Only executable code is pinned.
const code = source
  .replace(/<!--[\s\S]*?-->/g, '')
  .replace(/\/\*[\s\S]*?\*\//g, '')
  .replace(/(^|[^:])\/\/.*$/gm, '$1');

describe('TriageBoard stays queue-agnostic', () => {
  test('names no specific Workflow', () => {
    expect(code).not.toContain('user-feedback');
  });

  test('reads no queue-specific metadata field', () => {
    // `metadata.message` and `metadata.route` are feedback's shape and
    // live in the caller's snippet. The board reads only the agent
    // hand-off record, which it owns.
    expect(code).not.toMatch(/\[['"]message['"]\]/);
    expect(code).not.toMatch(/\[['"]route['"]\]/);
  });

  test('takes the queue as a prop', () => {
    expect(code).toMatch(/kind:\s*string/);
    expect(code).toMatch(/kind=\$\{encodeURIComponent\(kind\)\}/);
  });

  test('does not reimplement the fork rule', () => {
    // The rule moved to ./fork.ts so Flow could share it rather than
    // carry a second copy — it had already drifted once between this
    // board and the terminal queue reader. The board must import it,
    // not grow its own again.
    expect(code).toMatch(/from '\.\/fork'/);
    expect(code).not.toMatch(/function\s+(readFork|gatedStep)\b/);
  });

  // A STEP-kind comparison is the regression this guards. Scoped to
  // step-shaped receivers on purpose: `session.value.kind === 'ready'`
  // is a discriminated-union tag, not a registry kind name, and a
  // bare /\.kind ===/ flags it.
  const NO_STEP_KIND_MATCH = /\b(s|step|st)\.kind\s*===\s*['"]/;

  test('the board matches no step kind', () => {
    expect(code).not.toMatch(NO_STEP_KIND_MATCH);
  });

  // A closed packet must never render as queue contents. Placement
  // read only the fork step's state, so 182 closed packets sat under
  // their old route columns indistinguishable from live work — the
  // board showed 198 cards of which 16 were open, and was read as
  // "150+ still open" (David, 2026-08-19). The Job's own status must
  // outrank the program counter of one of its steps, and it must be
  // consulted BEFORE the fork step is.
  test('a closed Job is placed by its own status, ahead of its fork step', () => {
    const columnOf = code.slice(code.indexOf('function columnOf'));
    const statusCheck = columnOf.indexOf("j.status === 'closed'");
    const forkRead = columnOf.indexOf('forkStep(j)');
    expect(statusCheck).toBeGreaterThan(-1);
    expect(forkRead).toBeGreaterThan(-1);
    expect(statusCheck).toBeLessThan(forkRead);
  });
});

describe('the fork rule', () => {
  test('finds the parked step by its authority gate, not a step kind', () => {
    expect(forkSource).toMatch(/authority_role/);
    expect(forkSource).not.toMatch(/\b(s|step|st)\.kind\s*===\s*['"]/);
  });

  test('names no specific Workflow or disposition', () => {
    // Every value it works on comes from the registry. A literal here
    // would mean adding a disposition needs a code change.
    const forkCode = forkSource
      .replace(/\/\*[\s\S]*?\*\//g, '')
      .replace(/(^|[^:])\/\/.*$/gm, '$1');
    expect(forkCode).not.toContain('user-feedback');
    expect(forkCode).not.toMatch(/['"](reproduce|decline|duplicate|needs-info)['"]/);
  });
});

// The retention window has to be pushed into the QUERY, not applied to
// what comes back. The board asks for `limit=200`, so the server
// truncates before the client ever sees a row: a filter applied after
// the fetch drops finished cards that were already going to arrive
// while the live cards it was meant to protect have been cut off the
// end of the page. Client-side filtering here is not merely slower —
// it is wrong in the exact case the window exists for.
//
// Source-level for the same reason as everything above: the mistake is
// a plausible refactor ("just filter `jobs`"), and nothing else in the
// suite would go red for it.
describe('the terminal retention window', () => {
  test('is a query parameter, not a post-fetch filter', () => {
    expect(code).toContain('closed_within');
    // Whatever assembles the query, the window has to be in the same
    // request as the limit.
    expect(code).toMatch(/closed_within[\s\S]{0,200}?fetch\(`\/api\/jobs|limit[\s\S]{0,200}?closed_within/);
  });

  test('never filters the fetched rows by status in the client', () => {
    expect(code).not.toMatch(/jobs\s*\.\s*filter\([^)]*status/);
    expect(code).not.toMatch(/\.status\s*!==\s*['"]closed['"]/);
  });

  test('the window is a prop, so a queue can widen or disable it', () => {
    expect(code).toMatch(/terminalWindowDays\?:\s*number/);
    expect(code).toMatch(/terminalWindowDays\s*=\s*\d+/);
  });

  test('says what it is hiding and links to it', () => {
    // A board that silently drops history teaches that the history is
    // gone — the same confusion the window was built to end, pointing
    // the other way.
    expect(code).toMatch(/archived/);
    expect(source).toMatch(/href="\/jobs\?kind=/);
  });
});
