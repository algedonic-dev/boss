import { describe, expect, it } from 'bun:test';
import { readdirSync, readFileSync, statSync } from 'node:fs';
import { join } from 'node:path';
import { NOT_IN_SCOPE } from './withheld';

describe('NOT_IN_SCOPE', () => {
  it('says one thing everywhere', () => {
    expect(NOT_IN_SCOPE).toBe('not in your policy scope');
  });
});

// Backlog 1805bac0: a refusal by policy scope is the SERVER's flag — a
// `withheld` key or a `withheld` machine state — and never the server's
// words. The phrase detector this file used to hold matched "this
// caller's policy scope" in a reason, one fact kept twice in prose
// (CLAUDE.md 9a). This pin keeps it gone: no web source recognises a
// refusal by the server's phrase.
describe('no surface recognises a refusal by its words', () => {
  const src = join(import.meta.dir, '..');
  const sources = (dir: string): ReadonlyArray<string> =>
    readdirSync(dir).flatMap((name) => {
      const path = join(dir, name);
      if (statSync(path).isDirectory()) return sources(path);
      return /\.(ts|svelte)$/.test(name) && !/\.test\.ts$/.test(name) ? [path] : [];
    });

  it("no non-test source carries the server's refusal phrase", () => {
    const phrase = "this caller's policy scope";
    const offenders = sources(src).filter((path) => readFileSync(path, 'utf8').includes(phrase));
    expect(offenders).toEqual([]);
  });
});
