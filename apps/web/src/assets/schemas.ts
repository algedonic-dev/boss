// Runtime schemas for the assets API surfaces consumed by
// AssetPage. Mirrors `apps/web/src/assets/types.ts`.
//
// `phase` is an open string, here and in `types.ts`: the vocabulary is
// the Class registry's `(asset, phase)` rows, not a list in the SPA
// (backlog 53fecfc9), so an unfamiliar phase renders by its code.

import { z } from '../data/parseResponse';

export const AssetSchema = z.object({
  asset_id: z.string(),
  // Nullable — identity-first: a `registered` asset has no catalog
  // model until it is identified.
  sku: z.string().nullable(),
  phase: z.string(),
  holder_kind: z.string().nullable(),
  holder_id: z.string().nullable(),
  warranty_through: z.string().nullable(),
  open_ticket_count: z.number(),
  first_seen: z.string(),
  last_event_at: z.string(),
  oem_serial: z.string().nullable(),
});

/// `GET /api/assets/summary` (boss-assets' AssetsSummary). The page used
/// to cast it, so a 200 of any other shape — the smoke floor's `[]` —
/// painted every count as 0 with no failure line (backlog e1cb1ef3).
export const AssetsSummarySchema = z.object({
  phase_counts: z.array(z.object({ phase: z.string(), count: z.number() })),
  total_systems: z.number(),
  in_field_count: z.number(),
  open_tickets_total: z.number(),
  warranty_expiring_30d: z.number(),
  sku_counts: z.array(z.object({ sku: z.string(), count: z.number() })).optional(),
});

/// Asset events have a stable header (id/ts/actor_id/kind) plus
/// kind-specific tail fields. `passthrough()` is load-bearing —
/// the tail keys (holder_kind/holder_id, sku, source, oem_serial, etc.) are
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
