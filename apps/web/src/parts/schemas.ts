// Runtime schemas for the parts / inventory APIs.
//
// Mirrors `apps/web/src/parts/types.ts`. The TS types stay there
// for now (other modules import them by name); the zod schemas
// here are the runtime gatekeepers used by `fetchValidated`.
//
// See `apps/web/src/data/parseResponse.ts` for the helper.

import { z } from '../data/parseResponse';

/// `GET /api/inventory/items` — flat array.
export const InventoryItemSchema = z.object({
  part_sku: z.string(),
  bin: z.string(),
  on_hand: z.number(),
  allocated: z.number(),
  reorder_point: z.number(),
  reorder_qty: z.number(),
  trailing_90d_usage: z.number(),
});

export const InventoryItemListSchema = z.array(InventoryItemSchema);

const PurchaseOrderLineSchema = z.object({
  part_sku: z.string(),
  qty: z.number(),
  unit_cost_cents: z.number(),
  currency: z.string(),
});

/// `GET /api/inventory/orders` — flat array. `received_on` is
/// EXPLICITLY nullable: the production server returns null even
/// for received POs (the field tracks the line-item received
/// timestamp, not the header status). Pre-2026-05-24 the TS type
/// claimed `string | null` but downstream code occasionally
/// assumed non-null; the schema makes the wire shape canonical.
export const PurchaseOrderSchema = z.object({
  id: z.string(),
  vendor: z.string(),
  status: z.string(), // open enum — tenants extend status taxonomies
  placed_on: z.string(),
  expected_on: z.string(),
  received_on: z.string().nullable(),
  lines: z.array(PurchaseOrderLineSchema).default([]),
});

export const PurchaseOrderListSchema = z.array(PurchaseOrderSchema);
