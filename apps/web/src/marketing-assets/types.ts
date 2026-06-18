// Marketing asset types.

// A marketing-asset kind code. Kinds are tenant-extensible reference
// data owned by the Class registry (subject_kind='marketing-asset',
// member_attribute='kind'); the SPA reads the live list + labels from
// the registry (loadClasses / classesFor) rather than re-declaring them
// here. `kind` is nullable server-side — identity-first, an asset can
// be created unclassified — so render sites fall back when it's absent.
export type AssetKind = string;

export type MarketingAsset = {
  id: string;
  title: string;
  kind: AssetKind | null;
  description: string | null;
  file_url: string | null;
  tags: ReadonlyArray<string>;
  linked_device_skus: ReadonlyArray<string>;
  linked_account_ids: ReadonlyArray<string>;
  linked_campaign_ids: ReadonlyArray<string>;
  owner_id: string | null;
  brand_reviewed_by: string | null;
  brand_reviewed_at: string | null;
  supersedes_id: string | null;
  retired_at: string | null;
  created_at: string;
  updated_at: string;
};
