import { expect, test } from 'bun:test';
import { measureWidth } from './measure-width';

// A ResizeObserver whose callback the test fires, and a frame clock the
// test advances: the width is read in a LATER frame, never inside the
// observer's delivery, so a layout the width drives cannot re-trigger
// the observer in the same cycle (the ResizeObserver loop error that put
// Bun's error overlay over /it, R3a gate 7fd205de).
function harness() {
  const observers: { cb: () => void; observed: unknown; disconnected: boolean }[] = [];
  const frames = new Map<number, () => void>();
  let next = 1;
  const env = {
    observe: (cb: () => void) => {
      const o = { cb, observed: null as unknown, disconnected: false };
      observers.push(o);
      return {
        observe: (el: unknown) => { o.observed = el; },
        disconnect: () => { o.disconnected = true; },
      };
    },
    frame: (fn: () => void) => { const id = next++; frames.set(id, fn); return id; },
    cancel: (id: number) => { frames.delete(id); },
  };
  const flush = () => { const fns = [...frames.values()]; frames.clear(); fns.forEach((f) => f()); };
  const first = () => {
    const o = observers[0];
    if (o === undefined) throw new Error('no observer was made');
    return o;
  };
  const fire = () => first().cb();
  return { env, first, frames, flush, fire };
}

test('the width is set a frame after the resize, never inside the delivery', () => {
  const h = harness();
  const el = { clientWidth: 800 };
  const seen: number[] = [];
  measureWidth(el, (w) => seen.push(w), h.env);
  h.flush();
  expect(seen).toEqual([800]);
  el.clientWidth = 640;
  h.fire();
  expect(seen).toEqual([800]);
  h.flush();
  expect(seen).toEqual([800, 640]);
});

test('many resizes in one frame set the width once, and an unchanged width not at all', () => {
  const h = harness();
  const el = { clientWidth: 800 };
  const seen: number[] = [];
  measureWidth(el, (w) => seen.push(w), h.env);
  h.flush();
  el.clientWidth = 700;
  h.fire(); h.fire(); h.fire();
  expect(h.frames.size).toBe(1);
  h.flush();
  h.fire();
  h.flush();
  expect(seen).toEqual([800, 700]);
});

test('destroying it disconnects the observer and cancels a pending frame', () => {
  const h = harness();
  const el = { clientWidth: 800 };
  const seen: number[] = [];
  const action = measureWidth(el, (w) => seen.push(w), h.env);
  h.flush();
  el.clientWidth = 500;
  h.fire();
  action.destroy();
  h.flush();
  expect(seen).toEqual([800]);
  expect(h.first().disconnected).toBe(true);
});
