// A SOURCE SCAN READS ITS FILES SIDE BY SIDE, AND SAYS WHAT IT WAITS ON
// (backlog 3f421243, 2026-11-06).
//
// Train gate c773d182 (red train 5283152e) failed on
// src/it/yard/regions.test.ts's pin that no surface spells a region link
// itself: it reads every .ts and .svelte under src/ — 615 files, 5.4 MB —
// with one readFileSync after another, and ran 183 s against its 120 s
// budget. The same scan takes 0.27 s cold and 11 ms warm on a quiet
// worker. That web unit phase spent 230 s of its 333 s with every task
// stalled on io (io.full on the receipt's runtime_evidence), so each read
// queued behind the gate drive, and a serial scan pays that wait once per
// file. Its sibling read of one Rust file took 184 ms in the same run.
//
// So the reads go out SCAN_WIDTH at a time and wait in the drive's queue
// together, not in turn — measured cold on the worker (page cache dropped
// per file, three runs): serial 267-301 ms, 32 wide 72-86 ms. And the scan
// holds a budget of its own, set below its test's, so a scan that still
// runs out says which reads had not come back rather than leaving the
// runner to print a bare "timed out" that names nothing.
//
// Node-safe like dev-tree.ts: no `bun` import.

import { readFile } from 'node:fs/promises';
import { join } from 'node:path';

/// Reads in flight at once. Bounded, so a scan of a large tree never
/// asks for more open files than a container allows.
export const SCAN_WIDTH = 32;

export type SourceText = Readonly<{ path: string; text: string }>;

/// The text of each of `paths` (relative to `root`), in the order given.
/// Rejects when `budgetMs` passes first, naming how many were read and
/// the paths whose reads had not returned.
export async function readSources(
  root: string,
  paths: ReadonlyArray<string>,
  budgetMs: number,
  read: (path: string) => Promise<string> = (path) => readFile(path, 'utf8'),
): Promise<ReadonlyArray<SourceText>> {
  const texts: string[] = [];
  const waiting = new Set<string>();
  let next = 0;
  let done = 0;
  let expired = false;
  const lane = async (): Promise<void> => {
    while (!expired && next < paths.length) {
      const index = next;
      next += 1;
      const path = paths[index]!;
      waiting.add(path);
      const text = await read(join(root, path));
      waiting.delete(path);
      texts[index] = text;
      done += 1;
    }
  };
  let timer: ReturnType<typeof setTimeout> | undefined;
  const deadline = new Promise<never>((_, reject) => {
    timer = setTimeout(() => {
      expired = true;
      const named = [...waiting];
      const shown = named.slice(0, 10).join(', ');
      const more = named.length > 10 ? ` and ${named.length - 10} more` : '';
      reject(new Error(
        `source scan: read ${done} of ${paths.length} files under ${root} in ${budgetMs} ms; ` +
          `still waiting on: ${shown}${more}`,
      ));
    }, budgetMs);
  });
  try {
    await Promise.race([
      Promise.all(Array.from({ length: Math.min(SCAN_WIDTH, paths.length) }, lane)),
      deadline,
    ]);
  } finally {
    clearTimeout(timer);
  }
  return paths.map((path, index) => ({ path, text: texts[index]! }));
}
