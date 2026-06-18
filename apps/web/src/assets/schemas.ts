// Runtime schemas for the assets API surfaces consumed by
// AssetPage. Mirrors `apps/web/src/assets/types.ts`.
//
// `phase` is modeled as an open string here even though `types.ts`
// uses a closed union — the DB CHECK constraint can drift ahead of
// the SPA, and we'd rather render an unfamiliar phase than refuse
// the whole bundle. The TS type stays closed so new phases produce a
// compile error in switch-style consumers, which is the right
// granularity for catching that drift.

import { z } from '../data/parseResponse';

export const AssetSchema = z.object({
  asset_id: z.string(),
  // Nullable — identity-first: a `registered` asset has no catalog
  // model until it is identified.
  sku: z.string().nullable(),
  phase: z.string(),
  account_id: z.string().nullable(),
  warranty_through: z.string().nullable(),
  open_ticket_count: z.number(),
  first_seen: z.string(),
  last_event_at: z.string(),
  oem_serial: z.string().nullable(),
});

/// Asset events have a stable header (id/ts/actor_id/kind) plus
/// kind-specific tail fields. `passthrough()` is load-bearing —
/// the tail keys (account_id, sku, source, oem_serial, etc.) are
/// what the SPA's event-feed renderer reads.
export const AssetEventSchema = z.object({
  id: z.string(),
  asset_id: z.string(),
  ts: z.string(),
  actor_id: z.string().nullable(),
  kind: z.string(),
}).passthrough();

/// `GET /api/assets/{serial}` returns the composite shape, not the
/// flat Asset. `current_state` is nullable for an
/// unrecognized serial (the server returns 200 + null rather than
/// 404, see boss-assets/src/http.rs).
export const AssetDetailSchema = z.object({
  current_state: AssetSchema.nullable(),
  events: z.array(AssetEventSchema).default([]),
});

/// `GET /api/assets/{id}/parts` — flat array of Part rows.
/// Subject parts carry a typed reference; attribute parts carry a
/// kind-specific `value` blob the SPA narrows with a key check.
const SubjectPartSchema = z.object({
  part_kind: z.literal('subject'),
  subject_kind: z.string(),
  id: z.string(),
});
const AttributePartSchema = z.object({
  part_kind: z.literal('attribute'),
  key: z.string(),
  value: z.record(z.string(), z.unknown()),
});
export const AssetPartSchema = z.discriminatedUnion('part_kind', [
  SubjectPartSchema,
  AttributePartSchema,
]);
export const AssetPartListSchema = z.array(AssetPartSchema);
