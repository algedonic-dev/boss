// Brewery beer catalog — what Algedonic Ales sells through /shop.
//
// Catalog metadata sits in the SPA because (a) the brewery has 3-5
// active SKUs and changes them rarely, and (b) decoupling display
// from `/api/inventory/items` lets /shop render even if the inventory
// projection is briefly empty. Inventory STATE (on-hand,
// reserved) still comes from the live API; this module is just the
// "what does Pale Ale 1/2 BBL look like" layer.
//
// When/if a tenant needs to add SKUs without redeploying, promote
// this to the boss-classes / boss-catalog registry the same way the
// `account_type` taxonomy was lifted in 2026-04. The migration is
// mechanical: load via `/api/classes?subject_kind=product`,
// drop this file.
//
// FP-* SKU naming follows brewery convention: FP (Finished Product),
// brand slug (PALE, IPA, SEASONAL), package size (1-2-BBL, 1-6-BBL,
// 12OZ-CS). Matches the rows in examples/brewery/seeds/parts.toml.

export type PackageFormat =
  | { kind: 'keg'; size_bbl: number }      // 1/2 BBL or 1/6 BBL
  | { kind: 'case'; bottles: number; oz: number }; // 24-pack of 12oz

export type BreweryProduct = Readonly<{
  sku: string;
  brand: string;
  style: string;
  tagline: string;
  description: string;
  abv_pct: number;
  ibu: number;
  package: PackageFormat;
  /// Retail price for direct-shop sales. Wholesale orders use a
  /// separate price (built into JobKind metadata_defaults), so
  /// /shop can display a customer-facing price without coupling
  /// to wholesale pricing.
  unit_price_cents: number;
  /// `null` for permanent lineup; ISO date string for seasonal +
  /// limited releases — used by /shop to flag the "limited" badge.
  available_until: string | null;
}>;

export const BREWERY_PRODUCTS: ReadonlyArray<BreweryProduct> = [
  {
    sku: 'FP-PALE-1-2-BBL',
    brand: 'Algedonic Pale Ale',
    style: 'American Pale Ale',
    tagline: 'The everyday pour. House lineup, year-round.',
    description:
      'Crystal-clear amber, balanced bitterness, Cascade hop nose. ' +
      'Brewed every weekday on the 30-BBL house line. The keg is ' +
      'sized for taproom, restaurant, and event service.',
    abv_pct: 5.4,
    ibu: 38,
    package: { kind: 'keg', size_bbl: 0.5 },
    unit_price_cents: 19200,  // $192 / 1/2 BBL
    available_until: null,
  },
  {
    sku: 'FP-PALE-1-6-BBL',
    brand: 'Algedonic Pale Ale',
    style: 'American Pale Ale',
    tagline: 'Sixtel — for tap rotations + smaller venues.',
    description:
      'Same Pale Ale, sixtel-sized for tighter tap programs. ' +
      'Two cases-equivalent of pours per keg.',
    abv_pct: 5.4,
    ibu: 38,
    package: { kind: 'keg', size_bbl: 1 / 6 },
    unit_price_cents: 7200,  // $72 / 1/6 BBL
    available_until: null,
  },
  {
    sku: 'FP-IPA-1-2-BBL',
    brand: 'Algedonic IPA',
    style: 'West Coast IPA',
    tagline: 'Punchy citrus, dry finish. The flagship.',
    description:
      'Cascade + Centennial in the whirlpool, Citra dry-hop on ' +
      'fermentation drop. Crisp finish, no haze. Pairs with ' +
      'taproom-night burgers and barbecue trucks.',
    abv_pct: 7.2,
    ibu: 65,
    package: { kind: 'keg', size_bbl: 0.5 },
    unit_price_cents: 22400,  // $224
    available_until: null,
  },
  {
    sku: 'FP-IPA-1-6-BBL',
    brand: 'Algedonic IPA',
    style: 'West Coast IPA',
    tagline: 'Sixtel — for guest taps + cellar staff training.',
    description: 'Same IPA, sixtel-sized.',
    abv_pct: 7.2,
    ibu: 65,
    package: { kind: 'keg', size_bbl: 1 / 6 },
    unit_price_cents: 8400,  // $84
    available_until: null,
  },
  {
    sku: 'FP-SEASONAL-12OZ-CS',
    brand: 'Cascadia Reserve',
    style: 'Imperial Stout (barrel-aged)',
    tagline: 'Seasonal — released quarterly. Limited.',
    description:
      'A bourbon-barrel-aged imperial stout, packaged in 12oz ' +
      'bottles. 24 bottles per case. The barrel program runs ' +
      'four releases a year; once a release ships out, it does ' +
      'not come back.',
    abv_pct: 11.8,
    ibu: 55,
    package: { kind: 'case', bottles: 24, oz: 12 },
    unit_price_cents: 14400,  // $144 / case
    available_until: '2026-08-31',
  },
];

/// Lookup a single product by SKU. Returns undefined if the SKU
/// isn't in the brewery catalog (e.g. an old order referencing a
/// retired SKU).
export function findProduct(sku: string): BreweryProduct | undefined {
  return BREWERY_PRODUCTS.find((p) => p.sku === sku);
}

/// Display-friendly package label.
export function packageLabel(p: PackageFormat): string {
  if (p.kind === 'keg') {
    if (Math.abs(p.size_bbl - 0.5) < 0.01) return '1/2 BBL keg';
    if (Math.abs(p.size_bbl - 1 / 6) < 0.01) return '1/6 BBL keg (sixtel)';
    return `${p.size_bbl} BBL keg`;
  }
  return `${p.bottles}-pack · ${p.oz}oz bottles`;
}

/// Format USD cents as "$192" / "$72.50".
export function priceLabel(cents: number): string {
  const dollars = cents / 100;
  if (dollars === Math.floor(dollars)) {
    return `$${dollars.toLocaleString('en-US')}`;
  }
  return `$${dollars.toLocaleString('en-US', { minimumFractionDigits: 2, maximumFractionDigits: 2 })}`;
}
