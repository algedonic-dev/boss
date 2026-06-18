// Catalog types — hand-mirrored for Svelte surfaces.
// Keep in sync with `boss_catalog::types` in the Rust workspace.

// Free-text wrapper around a kebab-case Class-registry code.
// Tenants extend the supported categories by inserting Class rows
// under (subject_kind='asset', member_attribute='category');
// validation against the active Class set lives at the API boundary.
// Display strings come from `humanizeClassCode` against the same
// registry, not a hardcoded label table.
export type DeviceCategory = string;

export type Commerce = {
  list_price_new_cents: number;
  typical_refurb_price_cents: number | null;
  currency: string;
  lead_time_days: number | null;
  tagline: string;
  description: string;
  use_cases: ReadonlyArray<string>;
  hero_image: string | null;
};

export type FailureMode = {
  code: string;
  name: string;
  frequency: number;
  typical_fix: string;
};

export type ServiceProfile = {
  preventive_maintenance_hours: number;
  preventive_maintenance_interval_months: number;
  calibration_interval_months: number;
  required_skill_level: number;
  depot_required: boolean;
  common_failure_modes: ReadonlyArray<FailureMode>;
  pm_checklist: ReadonlyArray<string>;
};

export type SparePart = {
  part_sku: string;
  name: string;
  description: string;
  unit_price_cents: number;
  currency: string;
  lead_time_days: number;
  high_usage: boolean;
};

export type Consumable = {
  part_sku: string;
  name: string;
  description: string;
  unit_price_cents: number;
  currency: string;
  treatments_per_unit: number | null;
};

export type CatalogDocument = {
  kind: string;
  title: string;
  url: string;
  version: string | null;
  published: string | null;
  audience: string;
};

export type DeviceModel = {
  sku: string;
  name: string;
  manufacturer: string;
  model_year: number;
  category: DeviceCategory;
  // Tenant-defined kind-specific specs (typed-vs-extras
  // boundary): the platform stays neutral; tenants populate
  // whatever shape they need (networking-equipment
  // port counts, brewing-vessel capacity, printer ppm, etc.).
  extras: Record<string, unknown>;
  physical: Record<string, unknown>;
  regulatory: Record<string, unknown>;
  commerce: Commerce;
  service: ServiceProfile;
  spare_parts: ReadonlyArray<SparePart>;
  consumables: ReadonlyArray<Consumable>;
  documents: ReadonlyArray<CatalogDocument>;
  end_of_support: string | null;
  current_firmware: string | null;
};

/// Display label for a `DeviceCategory` code. Falls through to a
/// kebab-case → Title Case transformation when no explicit label is
/// registered. Kept here as a thin compatibility helper for surfaces
/// that imported `CATEGORY_LABEL` directly; new code should use
/// `humanizeClassCode` from `../people/types` instead.
export function categoryLabel(c: DeviceCategory): string {
  return c
    .split('-')
    .map((s) => (s.length > 0 ? s[0]!.toUpperCase() + s.slice(1) : s))
    .join(' ');
}

/// Compatibility shim — Proxy that resolves any string lookup to its
/// humanized form on demand. Equivalent shape to the old
/// `Record<DeviceCategory, string>` but no longer requires every
/// possible category to be enumerated. Drop the shim once every
/// caller has migrated to `categoryLabel(c)` directly.
export const CATEGORY_LABEL: Record<string, string> = new Proxy(
  {},
  {
    get(_target, prop: string) {
      return typeof prop === 'string' ? categoryLabel(prop) : undefined;
    },
  },
);
