// The chrome bar is the one piece of furniture every app shares, and
// it was rendered from three call sites that repeated their props by
// hand. They drifted twice: the step-focus bar shipped without
// `searchAppKinds`, so global search lost its app scoping on exactly
// the surface built for focused reading, and the Simulator's bar
// carried a hardcoded brand that a second tenant would have rendered
// as someone else's name.
//
// These pin the property the user actually notices: the same bar,
// with the same tenant identity, on every surface — including the two
// that render outside AppShell.

import { test, expect } from '@playwright/test';
import { mountPage } from '../smoke/_helpers';

/// Surfaces that render the chrome through different code paths:
/// a normal AppShell route, the full-page step route (rendered
/// OUTSIDE AppShell), and an IT surface.
const SURFACES = ['/ux/jobs', '/system', '/ux/views'] as const;

const MANIFEST = {
  display_name: 'Algedonic Ales',
  tenant_id: 'brewery',
  modules: {},
  labels: {},
};

test.describe('chrome bar', () => {
  test.beforeEach(async ({ page }) => {
    // `mountPage` does not install the smoke-mock backend, so the
    // manifest is mocked here — the brand comes from it now.
    await page.route(/\/api\/tenant\/manifest$/, (r) => r.fulfill({ json: MANIFEST }));
  });

  for (const path of SURFACES) {
    test(`shows the tenant's own name on ${path}`, async ({ page }) => {
      await mountPage(page, path);
      // The brand is split into wordmark + suffix, so assert on the
      // bar's text rather than a single node.
      const bar = page.locator('.perspective-tabs').first();
      await expect(bar).toContainText('Algedonic');
      await expect(bar).toContainText('Ales');
    });
  }

  // The bar is an unconditionally dark surface that sets no `color`,
  // so every control inside it has to declare its own. One used
  // `color: inherit` and so rendered in the DOCUMENT text colour —
  // invisible in light theme, fine in dark, which is why it survived
  // review. Reported from the field as "I can't read this feedback
  // button".
  //
  // Contrast is computable, so this checks the rendered result rather
  // than the stylesheet: whatever colour each control ends up with,
  // against whatever surface it actually sits on.
  test('every chrome control stays legible in light theme', async ({ page }) => {
    // Light is the theme it broke in: `inherit` resolves dark there.
    await page.emulateMedia({ colorScheme: 'light' });
    await mountPage(page, '/ux/jobs');

    const measured = await page.locator('.perspective-tabs').first().evaluate((bar) => {
      const channels = (c: string) => (c.match(/[\d.]+/g) ?? []).map(Number);
      const luminance = (c: string) => {
        const [r, g, b] = channels(c)
          .slice(0, 3)
          .map((v) => {
            const s = v / 255;
            return s <= 0.03928 ? s / 12.92 : Math.pow((s + 0.055) / 1.055, 2.4);
          });
        return 0.2126 * (r ?? 0) + 0.7152 * (g ?? 0) + 0.0722 * (b ?? 0);
      };
      /// The surface a control actually sits on. Two things make this
      /// more than "read the parent's background": a control may
      /// paint its own (the search field does, so comparing it to the
      /// bar would fail it wrongly), and a background may be
      /// TRANSLUCENT — the active tab's amber is 18% over near-black,
      /// which reads as a light colour if taken at face value and
      /// would fail white text that is in fact perfectly legible.
      /// So collect the stack down to the first opaque layer and
      /// composite it.
      const surface = (el: Element): string => {
        const layers: number[][] = [];
        let node: Element | null = el;
        while (node) {
          const ch = channels(getComputedStyle(node).backgroundColor);
          const alpha = ch.length > 3 ? (ch[3] ?? 1) : 1;
          if (alpha > 0) layers.push([ch[0] ?? 0, ch[1] ?? 0, ch[2] ?? 0, alpha]);
          if (alpha === 1) break;
          node = node.parentElement;
        }
        // Bottom-most opaque layer is the canvas; paint upward.
        let [r, g, b] = (layers[layers.length - 1] ?? [255, 255, 255]).slice(0, 3);
        for (let i = layers.length - 2; i >= 0; i--) {
          const [sr = 0, sg = 0, sb = 0, sa = 1] = layers[i] ?? [];
          r = sr * sa + (r ?? 0) * (1 - sa);
          g = sg * sa + (g ?? 0) * (1 - sa);
          b = sb * sa + (b ?? 0) * (1 - sa);
        }
        return `rgb(${Math.round(r ?? 0)}, ${Math.round(g ?? 0)}, ${Math.round(b ?? 0)})`;
      };

      return Array.from(bar.querySelectorAll('button, a'))
        .filter((el) => (el.textContent ?? '').trim().length > 0)
        .map((el) => {
          const fg = getComputedStyle(el).color;
          const bg = surface(el);
          const [lo, hi] = [luminance(fg), luminance(bg)].sort((a, b) => a - b);
          return {
            label: (el.textContent ?? '').trim().slice(0, 24),
            fg,
            bg,
            ratio: Math.round((((hi ?? 0) + 0.05) / ((lo ?? 0) + 0.05)) * 100) / 100,
          };
        });
    });

    expect(measured.length, 'no labelled controls found in the chrome bar').toBeGreaterThan(3);

    // 4.5:1 is the WCAG AA floor for normal-size text, which every
    // one of these is.
    const unreadable = measured.filter((m) => m.ratio < 4.5);
    expect(
      unreadable,
      `chrome controls below 4.5:1 contrast in light theme:\n${unreadable
        .map((m) => `  "${m.label}" ${m.fg} on ${m.bg} = ${m.ratio}:1`)
        .join('\n')}`,
    ).toEqual([]);
  });

  test('offers the same app tabs everywhere', async ({ page }) => {
    // The bar must not change shape as you navigate. Apps are
    // departments now, so the full list is as long as the org chart —
    // the bar pins Home, Simulator and YOUR department, and folds the
    // rest into More.
    //
    // The count assertion is the point: an earlier version of that
    // design also pinned whichever app you were currently in, which
    // made the set grow by one whenever you left your own department.
    // That is a second, drifted bar by another name, and this caught
    // it.
    const counts: number[] = [];
    for (const path of SURFACES) {
      await mountPage(page, path);
      counts.push(await page.locator('.perspective-tabs a[href]').count());
    }
    expect(new Set(counts).size, `tab counts differed across surfaces: ${counts}`).toBe(
      1,
    );
    // Home + Simulator at minimum; a signed-in operator also gets
    // their own department. This used to assert `> 4`, which encoded
    // the old seven-invented-app bar rather than anything true.
    expect(counts[0]).toBeGreaterThanOrEqual(2);
  });

  test('folds the other departments into More rather than dropping them', async ({
    page,
  }) => {
    // The apps not on the bar must still be reachable in one click —
    // "very few people need most of the Apps" is a reason to demote
    // them, never a reason to hide them.
    await mountPage(page, '/ux/jobs');
    const more = page.locator('.perspective-more-btn');
    await expect(more).toBeVisible();
    await more.click();
    const items = page.locator('.perspective-more-item');
    await expect(items.first()).toBeVisible();
    // Every department app that is not pinned shows up here.
    expect(await items.count()).toBeGreaterThan(4);
  });

  test('falls back to BOSS when the tenant has not named itself', async ({ page }) => {
    // A deployment with no [meta] in tenant.toml should read "BOSS",
    // not blank and not a brewery's name.
    await page.route(/\/api\/tenant\/manifest$/, (r) =>
      r.fulfill({ json: { modules: {}, labels: {} } }),
    );
    await mountPage(page, '/ux/jobs');
    const bar = page.locator('.perspective-tabs').first();
    await expect(bar).toContainText('BOSS');
    await expect(bar).not.toContainText('Algedonic');
  });
});
