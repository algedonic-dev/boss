// A container's width, read a frame AFTER it resizes (R3a, backlog
// 23a4b4d7). `bind:clientWidth` sets the width inside the
// ResizeObserver's delivery; on the tree floor that width drives the
// layout, which resizes the observed element in the same delivery, and
// the browser reports "ResizeObserver loop completed with undelivered
// notifications". In the mocked suite that error raised Bun's error
// overlay over /it?at=, which took the pointer from six feedback-triage
// specs (gate 7fd205de, diagnosed by Codex on 2a0c73be). Reading in the
// next frame leaves the delivery first; an unchanged width sets nothing.

type Observer = { observe(el: Element): void; disconnect(): void };
export type MeasureEnv = Readonly<{
  observe: (cb: () => void) => Observer;
  frame: (fn: () => void) => number;
  cancel: (id: number) => void;
}>;

const browser: MeasureEnv = {
  observe: (cb) => new ResizeObserver(() => cb()),
  frame: (fn) => requestAnimationFrame(() => fn()),
  cancel: (id) => cancelAnimationFrame(id),
};

/** Svelte action: `use:measureWidth={(w) => (width = w)}`. */
export function measureWidth(
  node: { clientWidth: number },
  set: (width: number) => void,
  env: MeasureEnv = browser,
): { destroy(): void } {
  let last: number | null = null;
  let pending: number | null = null;
  const read = () => {
    pending = null;
    const w = node.clientWidth;
    if (w !== last) {
      last = w;
      set(w);
    }
  };
  const schedule = () => {
    if (pending === null) pending = env.frame(read);
  };
  const observer = env.observe(schedule);
  observer.observe(node as Element);
  schedule();
  return {
    destroy() {
      observer.disconnect();
      if (pending !== null) env.cancel(pending);
      pending = null;
    },
  };
}
