// Runtime validation helper for HTTP responses.
//
// Why this exists: prior to 2026-05-24 the SPA used TypeScript
// casts at fetch boundaries (`(await r.json()) as DeviceModel`),
// which the compiler trusts even when the runtime payload has a
// different shape. The most common failure was nullable fields
// the cast claimed were non-null — e.g. DevicePage crashed with
// `Cannot read properties of null (reading 'wavelengths_nm')`
// when brewery's BREW-BARREL-* AssetModels arrived with
// `extras: null`.
//
// The fix: every fetch boundary parses the JSON through a zod
// schema. Schemas declare nullable / optional fields explicitly;
// the parse() call enforces them at runtime. Each domain owns
// its schemas in a sibling `schemas.ts` file (e.g.
// `apps/web/src/catalog/schemas.ts`).
//
// Adoption is incremental — old casts coexist with new
// parse-based helpers. Convert at the call sites that have
// actually surfaced bugs first; keep adding as new fetches land.

import { z, type ZodType } from 'zod';

/** Result of a validated fetch. Discriminated union so callers
 *  can handle parse failures explicitly without try/catch around
 *  every fetch. */
export type ParseResult<T> =
  | { kind: 'ok'; data: T }
  | { kind: 'invalid'; reason: string; raw: unknown }
  | { kind: 'error'; reason: string };

/** Fetch JSON + parse against a zod schema. Returns a discriminated
 *  union so the caller renders an honest error state instead of
 *  crashing on a bad payload.
 *
 *  Usage:
 *    const result = await fetchValidated('/api/catalog/models/foo', DeviceModelSchema);
 *    if (result.kind === 'ok') { use(result.data); }
 *    else if (result.kind === 'invalid') { renderInvalid(result.reason); }
 */
export async function fetchValidated<T>(
  url: string,
  schema: ZodType<T>,
  init?: RequestInit,
): Promise<ParseResult<T>> {
  let resp: Response;
  try {
    resp = await fetch(url, init);
  } catch (e) {
    return {
      kind: 'error',
      reason: e instanceof Error ? e.message : String(e),
    };
  }
  if (!resp.ok) {
    return {
      kind: 'error',
      reason: `HTTP ${resp.status} ${resp.statusText} on ${url}`,
    };
  }
  let raw: unknown;
  try {
    raw = await resp.json();
  } catch (e) {
    return {
      kind: 'error',
      reason: `JSON parse failed on ${url}: ${e instanceof Error ? e.message : String(e)}`,
    };
  }
  const parsed = schema.safeParse(raw);
  if (!parsed.success) {
    return {
      kind: 'invalid',
      reason: parsed.error.message,
      raw,
    };
  }
  return { kind: 'ok', data: parsed.data };
}

/** Validated variant of `fetchPaged` — fetches a list endpoint
 *  returning the `{data, total, limit, offset}` envelope and runs
 *  `itemSchema` against each row. Any other body shape (including a
 *  bare array) is a parse failure.
 *
 *  Returns the same `ParseResult` discriminated union as
 *  `fetchValidated`. Use this instead of `fetchPaged` from
 *  `data/paginated.ts` whenever you have a runtime schema for the
 *  row shape — the parse failure is observable rather than swallowed
 *  by `?? []`.
 */
export type PagedData<T> = Readonly<{
  data: ReadonlyArray<T>;
  total: number;
  limit: number;
  offset: number;
}>;

export async function fetchPagedValidated<T>(
  url: string,
  itemSchema: ZodType<T>,
  init?: RequestInit,
): Promise<ParseResult<PagedData<T>>> {
  const envelope = z.object({
    data: z.array(itemSchema),
    total: z.number().optional(),
    limit: z.number().optional(),
    offset: z.number().optional(),
  });
  const result = await fetchValidated(url, envelope, init);
  if (result.kind !== 'ok') return result;
  const body = result.data;
  return {
    kind: 'ok',
    data: {
      data: body.data,
      total: body.total ?? body.data.length,
      limit: body.limit ?? body.data.length,
      offset: body.offset ?? 0,
    },
  };
}

/** Convenience re-export so domain schema files can import
 *  everything from one place. */
export { z } from 'zod';
