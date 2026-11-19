import { describe, expect, it } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { PHONE_QUERY } from '@boss/web-kit/ui/phone';

// THE PHONE BREAKPOINT LIVES ONCE (design 62de32ae decision 12, car G;
// CLAUDE.md 9a). The media query that collapses the shell is CSS and the
// Department Map's gutter is CSS, and a phone between two numbers would
// get a desktop page inside a phone shell. This pin rode in
// phone-strip.test.ts until the phone strip retired with the transit map
// (R3, backlog 23a4b4d7); the strip went, the breakpoint did not.
describe('the phone breakpoint lives once', () => {
  it('the shell and the Department Map turn over at PHONE_QUERY and no other width', () => {
    const styles = readFileSync(join(import.meta.dir, '..', '..', 'styles.css'), 'utf8');
    const shell = [...styles.matchAll(/@media\s*(\([^)]*\))\s*\{[^@]*?\.app-shell\b/g)].map((m) => m[1]);
    expect(shell).toEqual([PHONE_QUERY]);
    const src = readFileSync(join(import.meta.dir, 'MapPage.svelte'), 'utf8');
    const queries = [...src.matchAll(/@media\s*(\([^)]*\))/g)].map((m) => m[1]);
    expect({ file: 'MapPage.svelte', queries }).toEqual({ file: 'MapPage.svelte', queries: [PHONE_QUERY] });
  });
});
