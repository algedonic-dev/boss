// Contrast measurement for the mocked suite.
//
// "Can you read this?" is the one design property a spec can answer
// honestly, and both specs that answer it were about to hand-roll the
// same 40 lines (CLAUDE.md §9a). It lives here once.
//
// The measurement runs in the page, not against the stylesheet, on
// purpose: an element's colour is the result of the cascade plus
// whatever inline style the component hand-rolled, and it is exactly
// the hand-rolled half that has produced both reported defects — a
// chrome control that took `color: inherit`, and a tag chip that
// painted itself a light ground under the app's light text.

import type { Locator } from '@playwright/test';

export type Measured = {
  /// The element's own text, truncated — so a failure names the thing
  /// on screen rather than a selector.
  readonly label: string;
  readonly fg: string;
  readonly bg: string;
  readonly ratio: number;
};

/**
 * Measure the text-to-surface contrast ratio of every element matching
 * `selector` inside `scope` that carries visible text.
 *
 * @param scope    the container to search within (and to composite
 *                 backgrounds up through)
 * @param selector CSS selector for the elements to measure
 */
export async function measureContrast(
  scope: Locator,
  selector: string,
): Promise<Measured[]> {
  return scope.evaluate((root: Element, sel: string) => {
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

    return Array.from(root.querySelectorAll(sel))
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
  }, selector);
}

/// 4.5:1 is the WCAG AA floor for normal-size text.
export const AA_FLOOR = 4.5;

/** Render a failing set as one readable block. */
export function describeUnreadable(measured: readonly Measured[]): string {
  return measured.map((m) => `  "${m.label}" ${m.fg} on ${m.bg} = ${m.ratio}:1`).join('\n');
}
