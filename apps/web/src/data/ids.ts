// Short display form for an id. Verbatim port of
// apps/web/src/data/ids.ts. Kept identical so phase-1 Svelte
// pages show ids the same way as the React equivalents — the
// UUID kind-prefix suffix bug fix from 2026-04-23 applies
// here too.

const UUID_RE =
  /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

/// A leading prefix of a packet id — 8 to 36 hex-and-hyphen characters
/// that are not a whole uuid — the shape the jobs API resolves on its
/// one-packet read (`is_id_prefix` in boss-jobs http/jobs.rs, cd7b0054)
/// and its stream does not (backlog 434b6e0d).
export function isIdPrefix(id: string): boolean {
  return /^[0-9a-f-]{8,36}$/i.test(id) && !UUID_RE.test(id);
}

export function shortId(id: string): string {
  if (UUID_RE.test(id)) {
    return id.replace(/-/g, '').slice(-8);
  }
  return id;
}
