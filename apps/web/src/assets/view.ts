// What /ux/assets is allowed to say, given which of its reads answered.
//
// WHY FUNCTIONS AND NOT $derived IN THE PAGE (page-audit 015c3935, gaps
// 3-8; the warehouse header's shape, 8b1deea2). Every count on the page
// fell back to `?? 0`, so a read in flight, a failed read and a summary
// of the wrong shape all painted an empty registry above the line that
// said otherwise. Which read may speak is the whole question, so each
// answer is one function with one test (view.test.ts).

import type { ReadState } from '../data/readState';
import { AssetsSummarySchema } from './schemas';
import type { Asset, AssetsSummary } from './types';

/// The summary body, or null when it is not a summary. It used to be
/// cast, and a 200 of `[]` read as zero of everything (e1cb1ef3).
export function summaryOf(body: unknown): AssetsSummary | null {
  const parsed = AssetsSummarySchema.safeParse(body);
  return parsed.success ? parsed.data : null;
}

export type PhaseTile = Readonly<{ phase: string; label: string; count: number }>;

/// A phase's label from the Class registry's `(asset, phase)` rows, or
/// the code itself when no row names it — a code is data, a typed
/// guess at a label is not.
export function phaseLabel(labels: ReadonlyMap<string, string>, code: string): string {
  return labels.get(code) ?? code;
}

/// One tile per phase the server counted, in the order it counted them.
/// The page kept its own PHASE_ORDER, a copy of a vocabulary the Class
/// registry holds (53fecfc9), and the copy left out `registered`, so
/// such assets sat in the header total and in no tile (3da7e008). The
/// summary counts every active phase in pipeline order (boss-assets
/// AssetLifecyclePhase::ORDER, held equal to the Class rows) and its
/// total is their sum, so these tiles always add up to the header.
export function phaseTiles(
  counts: AssetsSummary['phase_counts'],
  labels: ReadonlyMap<string, string>,
): ReadonlyArray<PhaseTile> {
  return counts.map((c) => ({ phase: c.phase, label: phaseLabel(labels, c.phase), count: c.count }));
}

/// Search reads every identifier the row carries. The placeholder said
/// "Serial" while the OEM serial was never read (8429a34a).
export function matchesQuery(a: Asset, query: string): boolean {
  if (!query) return true;
  const hay = [a.asset_id, a.sku, a.oem_serial].filter((s): s is string => s !== null).join(' ');
  return hay.toLowerCase().includes(query.toLowerCase());
}

export type AssetsHeader = Readonly<{ title: string; subtitle: string }>;

/// The header speaks for the summary alone: the list is capped at 500
/// and empty before it answers, so it cannot stand in for a count.
export function assetsHeader(
  summary: Readonly<{ read: ReadState; body: AssetsSummary | null }>,
  pageTitle: string,
): AssetsHeader {
  const s = summary.body;
  if (summary.read.kind === 'ok' && s) {
    const installed = s.phase_counts.find((p) => p.phase === 'installed')?.count ?? 0;
    return {
      title: `${s.total_systems.toLocaleString()} ${pageTitle}`,
      subtitle: `${installed.toLocaleString()} installed · ${s.open_tickets_total.toLocaleString()} open tickets · ${s.warranty_expiring_30d.toLocaleString()} warranties expiring (30d)`,
    };
  }
  if (summary.read.kind === 'loading') return { title: `Counting ${pageTitle}…`, subtitle: '' };
  return {
    title: `${pageTitle.charAt(0).toUpperCase()}${pageTitle.slice(1)} unavailable`,
    subtitle: "Couldn't load the asset summary",
  };
}

export type ListLine =
  | { kind: 'loading' }
  | { kind: 'failed'; error: string }
  | { kind: 'none-tracked' }
  | { kind: 'none-match' }
  | { kind: 'rows' };

/// "No assets match." was one line for an empty registry and for a
/// filter that excluded every loaded row (8429a34a).
export function listLine(read: ReadState, loaded: number, visible: number): ListLine {
  if (read.kind === 'loading') return { kind: 'loading' };
  if (read.kind === 'failed') return { kind: 'failed', error: read.error };
  if (loaded === 0) return { kind: 'none-tracked' };
  return visible === 0 ? { kind: 'none-match' } : { kind: 'rows' };
}

/// The capped list's hint. It said the kanban saw only the window and
/// that phase or search reach the rest; the counts are the summary's
/// (every asset) and both filters run over the loaded rows only —
/// /api/assets takes no phase or text parameter (1382989d).
export function overflowHint(summary: ReadState): string {
  return summary.kind === 'ok'
    ? 'The counts above cover every asset; search and the phase buttons look only at the rows loaded here.'
    : 'Search looks only at the rows loaded here.';
}
