// Short display form for an id. Verbatim port of
// apps/web/src/data/ids.ts. Kept identical so phase-1 Svelte
// pages show ids the same way as the React equivalents — the
// UUID kind-prefix suffix bug fix from 2026-04-23 applies
// here too.

const UUID_RE =
  /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

export function shortId(id: string): string {
  if (UUID_RE.test(id)) {
    return id.replace(/-/g, '').slice(-8);
  }
  return id;
}
