// Finished-product domain types — output goods the tenant
// produces and sells (kegs of beer, refurbished switches, ...).
// Sibling to the input-side `parts/types.ts`.

export type Product = {
  sku: string;
  name: string;
  product_kind: string;
  package_unit: string;
  description: string | null;
  metadata: Record<string, unknown>;
  active: boolean;
};

export type ProductInventory = {
  product_sku: string;
  location_id: string;
  on_hand: number;
  reserved: number;
  updated_at?: string | null;
};

export type ProductDetail = Product & {
  inventory: ReadonlyArray<ProductInventory>;
  total_on_hand: number;
};
