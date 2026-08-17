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
function fakeEl(): Record<string, unknown> {
  const el: Record<string, unknown> = {
    id: '',
    className: '',
    textContent: '',
    innerHTML: '',
    style: {},
    children: [] as unknown[],
    appendChild(c: unknown) {
      (el.children as unknown[]).push(c);
    },
    remove() {},
    addEventListener() {},
    querySelectorAll() {
      return [] as unknown[];
    },
    querySelector() {
      return null;
    },
  };
  return el;
}

/// Load the bundle with a stubbed window/document/fetch and hand back
/// whatever it registered. `fetch` rejects on purpose: the first paint
/// must not depend on the Job round trip, or the surface is blank for as
/// long as the network takes.
function loadBundle(): { kind: string; mount: MountFn } {
  let registered: { kind: string; mount: MountFn } | null = null;
  const g = globalThis as unknown as Record<string, unknown>;
  g.window = {
    __boss_register_step_plugin: (kind: string, mount: MountFn) => {
      registered = { kind, mount };
    },
  };
  g.document = {
    getElementById: () => null,
    createElement: fakeEl,
    head: { appendChild() {} },
  };
  g.fetch = () => Promise.reject(new Error('offline in test'));

  // eslint-disable-next-line no-new-func
  new Function(readFileSync(BUNDLE, 'utf8'))();

  if (!registered) throw new Error('bundle registered no plugin');
  return registered;
}

function render(step: Record<string, unknown>): { html: string; cleanup: unknown } {
  const { mount } = loadBundle();
  const container = fakeEl();
  const cleanup = mount(container, { step, jobId: 'job-1', onUpdate() {} });
  const first = (container.children as Record<string, unknown>[])[0];
  return { html: String(first?.innerHTML ?? ''), cleanup };
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
    const { html } = render(readyStep());
    expect(html).toContain('What was claimed');
    expect(html).toContain('What was measured');
  });

  test('spells out what each verdict causes', () => {
    // "accepted" and "unfounded" do not say what happens next, and that
    // was the other half of the complaint.
    const { html } = render(readyStep());
    expect(html).toContain('Accept — the correction is right');
    expect(html).toContain('Reject — the original claim held up');
    expect(html).toContain('Land it where the claim lives');
  });

  test('will not submit without a verdict', () => {
    // The field is required and the fork is total; an empty submit would
    // 400 at the API. Gate it in the surface instead.
    const { html } = render(readyStep());
    expect(html).toContain('disabled');
  });

  test('paints before the Job fetch resolves', () => {
    // fetch() rejects in this harness. A surface that waited for it
    // would be empty here, which is what the reviewer would see on a
    // slow link.
    const { html } = render(readyStep());
    expect(html.length).toBeGreaterThan(200);
  });

  test('a completed step shows its recorded verdict, not the form', () => {
    const { html } = render({
      ...readyStep(),
      status: 'completed',
      metadata: { verdict: 'accepted' },
    });
    expect(html).toContain('accepted');
    expect(html).not.toContain('Record verdict');
  });

  test('names missing evidence instead of rendering an empty box', () => {
    // An evidence step is supposed to carry all four fields. A blank one
    // means the correction was filed without its evidence — a reason to
    // send it back, not something to hide behind whitespace.
    const { html } = render(readyStep());
    expect(html).toContain('No claim recorded');
  });

  test('returns a cleanup function', () => {
    const { cleanup } = render(readyStep());
    expect(typeof cleanup).toBe('function');
  });
});
