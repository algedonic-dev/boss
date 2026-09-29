// The shape a read's 200 body must have, or a throw (backlogs
// e2679b23 (c) and 67825067).
//
// Packet 3fba9c35 swept the false-empty class out of the fetch: a
// refused status or a thrown network error is a FAILED read, never an
// empty one (remote.ts, paginated.ts). It stayed alive one layer down,
// in the parse. The inbox coerced any body that was not a list to [],
// and the marshalling board's three reads and /it/design's two coerced
// any body that was not the `{ data: [...] }` envelope to no rows — and
// no rows is what those pages paint as "Nothing is waiting on you",
// "Every watched station is clear." and "Nothing is waiting on a
// decision." A 200 that does not say "no rows" is not a read that did.
//
// So these are the shape checks, one per wire shape, sharing one way of
// saying what came back instead. Each throws on any other body; fetchRemote
// (or a page's own catch) turns the throw into the failure line, and the
// message names the read the way fetchRemote names a refused status —
// `<path>: HTTP 200, but …` beside `<path>: HTTP 500` — so the two
// failures read alike.
//
// They check the outer shape and nothing inside a row: each reader still
// parses its own fields, because that is where the page's own words about
// a missing field live.

/** What came back instead, in words: "a list", "null", "a string". */
function described(body: unknown): string {
  if (body === null) return 'null';
  if (Array.isArray(body)) return 'a list';
  const t = typeof body;
  return /^[aeiou]/.test(t) ? `an ${t}` : `a ${t}`;
}

/// A read of `path` whose body must be a bare list. `[]` is the ONLY
/// empty. (The inbox's parse, 2026-09-25, generalised here.)
export function readList(path: string, raw: unknown): ReadonlyArray<unknown> {
  if (!Array.isArray(raw)) {
    throw new Error(`${path}: HTTP 200, but the body is ${described(raw)}, not a list`);
  }
  return raw;
}

export type Envelope = Readonly<{
  /** The whole body, for the fields a read carries beside its rows
   *  (`distinct_packets`, `window_hours`, `steps`, `lens`, …). */
  body: Readonly<Record<string, unknown>>;
  data: ReadonlyArray<Readonly<Record<string, unknown>>>;
}>;

/// A read of `path` whose body must be the `{ data: [...] }` envelope:
/// its rows and the body they came in, or a throw naming the path and
/// what the body was instead. `{ data: [] }` is the ONLY empty.
export function readEnvelope(path: string, raw: unknown): Envelope {
  if (raw !== null && typeof raw === 'object' && !Array.isArray(raw)) {
    const body = raw as Readonly<Record<string, unknown>>;
    if (Array.isArray(body.data)) {
      return { body, data: body.data as ReadonlyArray<Readonly<Record<string, unknown>>> };
    }
  }
  throw new Error(`${path}: HTTP 200, but the body is ${describedObject(raw)}, not a {data: [...]} envelope`);
}

function describedObject(raw: unknown): string {
  return raw !== null && typeof raw === 'object' && !Array.isArray(raw)
    ? 'an object with no data list'
    : described(raw);
}

/// A read of `path` whose body may be EITHER list shape — a bare list or
/// the envelope — as the catalog, inventory and vendor reads have
/// answered both. Each page coerced any other body to no rows
/// (`Array.isArray(b) ? b : (b.data ?? [])`), which its empty state then
/// called an empty source (backlog 0ef5e008).
export function readRows(path: string, raw: unknown): ReadonlyArray<unknown> {
  if (Array.isArray(raw)) return raw;
  if (raw !== null && typeof raw === 'object' && Array.isArray((raw as { data?: unknown }).data)) {
    return (raw as { data: ReadonlyArray<unknown> }).data;
  }
  throw new Error(
    `${path}: HTTP 200, but the body is ${describedObject(raw)}, not a list or a {data: [...]} envelope`,
  );
}
