// The kinds the new-Job form offers on a page mounted for a department.
//
// "Start a new Job" on /ux/sales read GET /api/workflows unfiltered, so
// the Sales pipeline offered all 64 live kinds, two of them Sales's
// (backlog dc06c0fc, page audit 1e9283fc gap 4). A department's kinds
// are declared on their workflow row, under `metadata.department` — the
// same declaration, read by the same rule, as the server's department
// join (boss-jobs `department::carried`: a non-empty string, or no
// department). The listing on the same page already narrows by it.
//
// The registered ad-hoc row rides along: it declares no department
// because it serves every one, and the page's own "Create Ad Hoc Job"
// preselects it, which a picker without it could not show (a399613d).
// A department nothing declares offers no other kind — never the whole
// registry, which is the confident wrong answer this replaced.

import { AD_HOC_KIND } from './adHoc';

type DeclaringRow = { kind: string; metadata?: Record<string, unknown> | null };

/** The department a workflow row declares, if any. */
function declared(row: DeclaringRow): string | null {
  const d = row.metadata?.['department'];
  return typeof d === 'string' && d !== '' ? d : null;
}

/** The rows the picker offers: every row when `department` is empty
 *  (a page mounted for no department), else the rows declaring it and
 *  the ad-hoc row, in the registry's order. */
export function kindsForDepartment<K extends DeclaringRow>(
  rows: ReadonlyArray<K>,
  department: string,
): K[] {
  if (!department) return [...rows];
  return rows.filter((r) => r.kind === AD_HOC_KIND || declared(r) === department);
}
