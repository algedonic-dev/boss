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

/// Comments deliberately discuss feedback — it is where the component
/// came from and why the seam exists. Only executable code is pinned.
const code = source
  .replace(/<!--[\s\S]*?-->/g, '')
  .replace(/\/\*[\s\S]*?\*\//g, '')
  .replace(/(^|[^:])\/\/.*$/gm, '$1');

describe('TriageBoard stays queue-agnostic', () => {
  test('names no specific JobKind', () => {
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

  test('finds the parked step by its authority gate, not a step kind', () => {
    expect(code).toMatch(/authority_role/);
    // A STEP-kind comparison is the regression this guards. Scoped to
    // step-shaped receivers on purpose: `session.value.kind === 'ready'`
    // is a discriminated-union tag, not a registry kind name, and a
    // bare /\.kind ===/ flags it.
    expect(code).not.toMatch(/\b(s|step|st)\.kind\s*===\s*['"]/);
  });
});
