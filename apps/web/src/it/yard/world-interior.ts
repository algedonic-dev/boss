// MapPage still uses this predicate to choose a queue board rather
// than FloorDeck. The callback-only platform builders retired with
// their region-map consumers (cfd67b9e).
export const PLATFORM_REGIONS: ReadonlyArray<string> = ['receiving', 'marshalling', 'shop-floor'];

export function hasPlatforms(region: string): boolean {
  return PLATFORM_REGIONS.includes(region);
}
