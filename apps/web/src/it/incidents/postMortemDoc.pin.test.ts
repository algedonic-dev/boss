// The post-mortem's three tables live twice, and this test is what
// keeps the two copies one fact (CLAUDE.md §9a; backlog 3b9544e1, gap 8
// of page-audit 9b9849f5).
//
// postMortemDoc.ts renders the archive in the SPA; the step plugin
// infra/step-plugins/incident-review.js renders the same document for
// the reviewer. A plugin is a standalone bundle served by the gateway
// and imports nothing from the SPA, so the two cannot share a module —
// decided on the packet 2026-09-24. Both files said "Change one, change
// both" and nothing read both: they had already drifted (PLUMBING lived
// only in the plugin until 434b6e0d gave the archive its own copy).
//
// So this reads the plugin's SOURCE and compares its KNOWN_LABELS,
// READING_ORDER and PLUMBING literals with the page's exported tables.
// Each failure names the entry that differs. A literal this reader
// cannot parse whole is a failure too — a table it read as empty, or
// read half of, would pass the comparison while checking nothing.

import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { KNOWN_LABELS, PLUMBING, READING_ORDER } from './postMortemDoc';

const PLUGIN = new URL('../../../../../infra/step-plugins/incident-review.js', import.meta.url);
const pluginSource = (): string => readFileSync(PLUGIN, 'utf8');

/// The one literal `opener ... closer` in the source. Exactly one: a
/// second declaration would leave it unclear which copy the plugin uses.
function literal(source: string, opener: string, closer: string): string {
  const starts = source.split(opener).length - 1;
  if (starts !== 1) {
    throw new Error(`incident-review.js declares \`${opener.trim()}\` ${starts} times, expected once`);
  }
  const from = source.indexOf(opener) + opener.length;
  const to = source.indexOf(closer, from);
  if (to < 0) throw new Error(`\`${opener.trim()}\` in incident-review.js has no \`${closer}\``);
  return source.slice(from, to);
}

/// Every match of `item` in `body`, refusing any text the matches do not
/// account for beyond separators — a double-quoted or computed entry
/// must fail here, not drop out of the comparison.
function entries(body: string, item: RegExp, name: string): ReadonlyArray<RegExpMatchArray> {
  const found = [...body.matchAll(item)];
  const rest = body.replace(item, '').replace(/[\s,]/g, '');
  if (rest !== '') throw new Error(`${name} in incident-review.js holds text this pin cannot read: ${rest}`);
  if (found.length === 0) throw new Error(`${name} in incident-review.js read as empty`);
  return found;
}

function pluginTables(source: string): {
  labels: Record<string, string>;
  order: ReadonlyArray<string>;
  plumbing: ReadonlyArray<string>;
} {
  // Both groups are non-optional, so a match always carries them; the
  // defaults only satisfy noUncheckedIndexedAccess.
  const labels = Object.fromEntries(
    entries(literal(source, 'const KNOWN_LABELS = {', '};'), /(\w+):\s*'([^']*)'/g, 'KNOWN_LABELS').map(
      ([, key = '', label = '']) => [key, label] as const,
    ),
  );
  const strings = (body: string, name: string) =>
    entries(body, /'([^']*)'/g, name).map(([, value = '']) => value);
  return {
    labels,
    order: strings(literal(source, 'const READING_ORDER = [', '];'), 'READING_ORDER'),
    plumbing: strings(literal(source, 'const PLUMBING = new Set([', ']);'), 'PLUMBING'),
  };
}

/// Each key whose label differs or exists on one side only.
function labelDrift(page: Readonly<Record<string, string>>, plugin: Readonly<Record<string, string>>) {
  const keys = [...new Set([...Object.keys(page), ...Object.keys(plugin)])];
  return keys
    .filter((k) => page[k] !== plugin[k])
    .map((k) => `KNOWN_LABELS.${k}: page ${JSON.stringify(page[k])}, plugin ${JSON.stringify(plugin[k])}`);
}

/// Each position where the two orders disagree — order is the content.
function orderDrift(page: ReadonlyArray<string>, plugin: ReadonlyArray<string>) {
  return Array.from({ length: Math.max(page.length, plugin.length) }, (_, i) => i)
    .filter((i) => page[i] !== plugin[i])
    .map((i) => `READING_ORDER[${i}]: page ${JSON.stringify(page[i])}, plugin ${JSON.stringify(plugin[i])}`);
}

/// Each key in one set and not the other — the plugin holds a Set, so
/// order carries nothing here.
function setDrift(page: ReadonlyArray<string>, plugin: ReadonlyArray<string>) {
  return [
    ...page.filter((k) => !plugin.includes(k)).map((k) => `PLUMBING: ${k} only on the page`),
    ...plugin.filter((k) => !page.includes(k)).map((k) => `PLUMBING: ${k} only in the plugin`),
  ];
}

describe('postMortemDoc.ts and incident-review.js hold one set of tables', () => {
  const plugin = pluginTables(pluginSource());

  test('KNOWN_LABELS: every key carries the same label in both', () => {
    expect(labelDrift(KNOWN_LABELS, plugin.labels)).toEqual([]);
  });

  test('READING_ORDER: the same slots in the same order', () => {
    expect(orderDrift(READING_ORDER, plugin.order)).toEqual([]);
  });

  test('PLUMBING: the same keys are kept out of the findings', () => {
    expect(setDrift(PLUMBING, plugin.plumbing)).toEqual([]);
  });

  // The control, kept: one label changed in the plugin's text is a named
  // failure, so the pin above is strong enough to see the drift it guards.
  test('a label changed in the plugin source is reported by its key', () => {
    const drifted = pluginSource().replace("root_cause: 'Root cause'", "root_cause: 'Cause'");
    expect(labelDrift(KNOWN_LABELS, pluginTables(drifted).labels)).toEqual([
      'KNOWN_LABELS.root_cause: page "Root cause", plugin "Cause"',
    ]);
  });

  test('an entry the reader cannot parse fails the read rather than vanishing', () => {
    const drifted = pluginSource().replace("'human_only',", '"human_only",');
    expect(() => pluginTables(drifted)).toThrow('PLUMBING in incident-review.js holds text');
  });
});
