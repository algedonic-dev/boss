// The correction-verdict plugin bundle actually mounts, and renders the
// thing it exists to render.
//
// WHY A TEST FOR A FILE IN infra/. Step plugin bundles are shipped as
// static JS and loaded at runtime by the SPA, which means nothing
// compiles them and nothing type-checks them. A typo in the bundle is
// invisible until a person opens the one surface they need in order to
// act on a step — and `StepSurface` prefers a registered plugin over its
// built-in surface, so a broken bundle does not degrade to the generic
// form, it just renders nothing. That is the worst place for an
// unchecked artefact to be.
//
// So this loads the real bundle against a stubbed host and asserts the
// contract on both sides: that it registers the kind the migration
// declares, and that mount() produces the comparison the step is for.
//
// The sibling lint `infra/lint/step-plugin-bundle-exists.sh` covers the
// other half of the two-artefact problem — a registry row whose bundle
// is missing from the tree entirely.

import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';

const BUNDLE = new URL('../../../../infra/step-plugins/correction-verdict.js', import.meta.url);
const MIGRATION = new URL(
  '../../../../infra/postgres/schema/146-correction-verdict-plugin.sql',
  import.meta.url,
);

type MountFn = (
  container: unknown,
  props: { step: unknown; jobId: string; onUpdate: () => void },
) => unknown;

/// A DOM stand-in with only what the bundle touches. Deliberately thin:
/// if the bundle starts reaching for something else, this throws rather
/// than silently doing nothing, which is the signal we want.
///
/// It has no innerHTML at all (backlog 4a359b51, 2026-09-27): the
/// surface is built as nodes, and an assignment of markup is refused
/// here as loudly as CodeQL refused the sign-off bundle's.
class FakeNode {
  tagName: string;
  id = '';
  className = '';
  textContent = '';
  disabled = false;
  checked = false;
  value = '';
  style: Record<string, string> = {};
  attrs: Record<string, string> = {};
  children: FakeNode[] = [];
  constructor(tag = '#text') {
    this.tagName = tag.toUpperCase();
  }
  set innerHTML(_: string) {
    throw new Error('the bundle assigned innerHTML');
  }
  appendChild(c: FakeNode) {
    this.children.push(c);
    return c;
  }
  append(...cs: Array<FakeNode | string>) {
    for (const c of cs) {
      if (typeof c === 'string') {
        const t = new FakeNode();
        t.textContent = c;
        this.children.push(t);
      } else this.children.push(c);
    }
  }
  replaceChildren(...cs: FakeNode[]) {
    this.children = cs;
  }
  setAttribute(k: string, v: string) {
    this.attrs[k] = v;
  }
  remove() {}
  addEventListener() {}
}

function walk(n: FakeNode, out: FakeNode[] = []): FakeNode[] {
  out.push(n);
  for (const c of n.children) walk(c, out);
  return out;
}

/// Load the bundle with a stubbed window/document/fetch and hand back
/// whatever it registered. `fetch` rejects on purpose: the first paint
/// must not depend on the Job round trip, or the surface is blank for as
/// long as the network takes.
function loadBundle(): { kind: string; mount: MountFn } {
  let registered: { kind: string; mount: MountFn } | null = null;
  const g = globalThis as unknown as Record<string, unknown>;
  g.Node = FakeNode;
  g.window = {
    __boss_register_step_plugin: (kind: string, mount: MountFn) => {
      registered = { kind, mount };
    },
  };
  g.document = {
    getElementById: () => null,
    createElement: (tag: string) => new FakeNode(tag),
    head: new FakeNode('head'),
  };
  g.fetch = () => Promise.reject(new Error('offline in test'));

  // eslint-disable-next-line no-new-func
  new Function(readFileSync(BUNDLE, 'utf8'))();

  if (!registered) throw new Error('bundle registered no plugin');
  return registered;
}

function render(step: Record<string, unknown>): { text: string; root: FakeNode; cleanup: unknown } {
  const { mount } = loadBundle();
  const container = new FakeNode('div');
  const cleanup = mount(container, { step, jobId: 'job-1', onUpdate() {} });
  const root = container.children[0] ?? new FakeNode('div');
  const text = walk(root)
    .map((n) => n.textContent)
    .join(' ');
  return { text, root, cleanup };
}

const readyStep = () => ({
  id: 'step-1',
  kind: 'correction-verdict',
  status: 'ready',
  fields: [],
  metadata: {},
});

describe('the correction-verdict bundle', () => {
  test('registers the kind the migration declares', () => {
    const { kind } = loadBundle();
    expect(kind).toBe('correction-verdict');
    // Both places name the same kind, or the plugin never mounts.
    expect(readFileSync(MIGRATION, 'utf8')).toContain("'correction-verdict', 1, 'active'");
  });

  test('renders the claim against the measurement', () => {
    // This IS the fix. David could not see the trade-off because the
    // claim and the measurement lived on a different step.
    const { text } = render(readyStep());
    expect(text).toContain('What was claimed');
    expect(text).toContain('What was measured');
  });

  test('spells out what each verdict causes', () => {
    // "accepted" and "unfounded" do not say what happens next, and that
    // was the other half of the complaint.
    const { text } = render(readyStep());
    expect(text).toContain('Accept — the correction is right');
    expect(text).toContain('Reject — the original claim held up');
    expect(text).toContain('Land it where the claim lives');
  });

  test('will not submit without a verdict', () => {
    // The field is required and the fork is total; an empty submit would
    // 400 at the API. Gate it in the surface instead.
    const { root } = render(readyStep());
    const buttons = walk(root).filter((n) => n.tagName === 'BUTTON');
    expect(buttons.map((b) => walk(b).map((n) => n.textContent).join(''))).toEqual(['Record verdict']);
    expect(buttons.map((b) => b.disabled)).toEqual([true]);
  });

  test('paints before the Job fetch resolves', () => {
    // fetch() rejects in this harness. A surface that waited for it
    // would be empty here, which is what the reviewer would see on a
    // slow link.
    const { text } = render(readyStep());
    expect(text.trim().length).toBeGreaterThan(200);
  });

  test('a completed step shows its recorded verdict, not the form', () => {
    const { text } = render({
      ...readyStep(),
      status: 'completed',
      metadata: { verdict: 'accepted' },
    });
    expect(text).toContain('accepted');
    expect(text).not.toContain('Record verdict');
  });

  test('names missing evidence instead of rendering an empty box', () => {
    // An evidence step is supposed to carry all four fields. A blank one
    // means the correction was filed without its evidence — a reason to
    // send it back, not something to hide behind whitespace.
    const { text } = render(readyStep());
    expect(text).toContain('No claim recorded');
  });

  test('a recorded verdict carrying markup is one text node, as written', () => {
    // The real-browser half is tests/mocked/step-plugins-draw-page-text-
    // as-text.mocked.spec.ts; this pins that the string reaches the DOM
    // whole, as a Text node's content (4a359b51).
    const payload = '<img src=x onerror=alert(1)>';
    const { root } = render({ ...readyStep(), status: 'completed', metadata: { verdict: payload } });
    const strong = walk(root).find((n) => n.tagName === 'STRONG');
    expect(strong?.children.map((c) => [c.tagName, c.textContent])).toEqual([['#TEXT', payload]]);
  });

  test('returns a cleanup function', () => {
    const { cleanup } = render(readyStep());
    expect(typeof cleanup).toBe('function');
  });
});
