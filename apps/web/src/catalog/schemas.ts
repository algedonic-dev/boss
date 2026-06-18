// Runtime schemas for catalog API responses.
//
// Pair with `apps/web/src/catalog/types.ts` — the schemas here
// are the source of truth for what the wire payload actually
// looks like; the TypeScript types in `types.ts` are inferred
// from these schemas, not the other way around.
//
// Reason this exists: TypeScript casts at fetch boundaries
// (`(await r.json()) as DeviceModel`) silently trust the
// declared shape. When the server's actual JSON has nullable
// fields (brewery's `BREW-BARREL-*` AssetModels return
// `extras: null` / `physical: null` / `regulatory: null`), the
// cast lies; downstream `.foo` access crashes. Zod's `.nullable()`
// marker forces every consumer to handle the null case.
//
// See `apps/web/src/data/parseResponse.ts::fetchValidated` for
// how callers consume these.

import { z } from '../data/parseResponse';

const SparePartSchema = z.object({
  part_sku: z.string(),
  name: z.string(),
  description: z.string(),
  unit_price_cents: z.number(),
  currency: z.string(),
  lead_time_days: z.number(),
  high_usage: z.boolean(),
});

const ConsumableSchema = z.object({
  part_sku: z.string(),
  name: z.string(),
  description: z.string(),
  unit_price_cents: z.number(),
  currency: z.string(),
  treatments_per_unit: z.number().nullable(),
});

const CatalogDocumentSchema = z.object({
  kind: z.string(),
  title: z.string(),
  url: z.string(),
  version: z.string().nullable(),
  published: z.string().nullable(),
  audience: z.string(),
});

const CommerceSchema = z.object({
  list_price_new_cents: z.number(),
  typical_refurb_price_cents: z.number().nullable(),
  currency: z.string(),
  lead_time_days: z.number().nullable(),
  tagline: z.string().default(''),
  description: z.string().default(''),
  use_cases: z.array(z.string()).default([]),
  hero_image: z.string().nullable(),
});

const FailureModeSchema = z.object({
  code: z.string(),
  name: z.string(),
  frequency: z.number(),
  typical_fix: z.string(),
});

const ServiceProfileSchema = z.object({
  preventive_maintenance_hours: z.number(),
  preventive_maintenance_interval_months: z.number(),
  calibration_interval_months: z.number(),
  required_skill_level: z.number(),
  depot_required: z.boolean(),
  common_failure_modes: z.array(FailureModeSchema).default([]),
  pm_checklist: z.array(z.string()).default([]),
});

/// Canonical wire shape for `GET /api/catalog/models/{sku}` +
/// `GET /api/catalog/models`. Note: `extras`, `physical`, and
/// `regulatory` are EXPLICITLY NULLABLE — brewery's
/// BREW-BARREL-* models return null for all three, and the
/// pre-2026-05-24 SPA crashed on the first field access into
/// them. Consumers must `?? {}` before reading.
export const DeviceModelSchema = z.object({
  sku: z.string(),
  name: z.string(),
  manufacturer: z.string(),
  model_year: z.number(),
  category: z.string(), // open string — categories are tenant-extensible
  extras: z.record(z.string(), z.unknown()).nullable(),
  physical: z.record(z.string(), z.unknown()).nullable(),
  regulatory: z.record(z.string(), z.unknown()).nullable(),
  commerce: CommerceSchema,
  service: ServiceProfileSchema,
  spare_parts: z.array(SparePartSchema).default([]),
  consumables: z.array(ConsumableSchema).default([]),
  documents: z.array(CatalogDocumentSchema).default([]),
  end_of_support: z.string().nullable(),
  current_firmware: z.string().nullable(),
});

export type DeviceModel = z.infer<typeof DeviceModelSchema>;

export const DeviceModelListSchema = z.array(DeviceModelSchema);

/// `/api/catalog/parts` shape. Tenants (brewery) seed parts
/// directly into a standalone `parts` table; the response is a
/// flat array with no system-model linkage.
export const CatalogPartSchema = z.object({
  part_sku: z.string(),
  name: z.string(),
  description: z.string(),
  unit_price_cents: z.number(),
  currency: z.string(),
  lead_time_days: z.number(),
});

export type CatalogPart = z.infer<typeof CatalogPartSchema>;

export const CatalogPartListSchema = z.array(CatalogPartSchema);
