// The Credentials tab's read model — the credentials registry drawn, and
// its lifecycle offered as packets (backlog 851259b9; design 76155676
// step 3, David 2026-09-27: "an IT page with all our credentials for
// access to external dependencies").
//
// THE PAGE HOLDS NO VALUE AND READS NONE. `GET /api/credentials` serves
// locations, never contents (boss-jobs credentials/mod.rs: "a row carries
// LOCATIONS, never contents"), and `parseCredentials` keeps only the
// registry's own fields, so a body that grew a value would still not
// reach the DOM. The one thing a viewer types — a new credential's id —
// is refused when it is shaped like a token rather than a name.
//
// PROVENANCE, from data, not from the issuer's prose. Three answers:
//   root   — the value lives in the broker's root Secret, which
//            boss-credential-broker.yaml names as the root material
//            David places by hand (a ceremony, never a broker product);
//   broker — an active dispatcher rule runs a `credential.rotate.*`
//            handler for this id, so the broker mints and installs it;
//   hand   — neither: a human mints and places it, which is exactly
//            the set design 76155676 moves under the broker.
// Which root a broker credential is minted FROM is not a registry fact —
// the handler reads it from its own env (boss-dispatcher config.rs) — so
// the page draws it as a gap rather than inferring it from the handler's
// name. A rule read that fails is `unknown`, never `hand`.
//
// ABSENCE IS NEVER A VALUE. No consumer, no recorded rotation, empty
// scopes (the registry's "unverified", boss credential list's
// `(unverified)`) and a scheduled policy with no interval are GAPS, drawn
// as missing. A registry read that fails is `failed`, not an empty
// registry — the rule `data/remote.ts` exists for.

import { fetchRemote, type Remote } from '../../data/remote';

export type Settled<T> = Exclude<Remote<T>, { kind: 'loading' }>;

const str = (v: unknown): string => (typeof v === 'string' ? v : '');
const rec = (v: unknown): Record<string, unknown> | null =>
  v !== null && typeof v === 'object' && !Array.isArray(v) ? (v as Record<string, unknown>) : null;

export type Consumer = Readonly<{ kind: string; location: string }>;

/// One registry row (boss-jobs `CredentialRow`), camel-cased.
export type Credential = Readonly<{
  /// The durable identity; survives rotation, and is the Subject a
  /// rotate-a-credential packet opens on.
  id: string;
  kind: string;
  /// Who mints it, as the declaration spells it.
  issuer: string;
  /// Whose authority it carries.
  principal: string;
  /// As the issuer spells them; empty = unverified.
  scopes: ReadonlyArray<string>;
  /// WHERE the value lives. Never the value.
  storageLocation: string;
  consumers: ReadonlyArray<Consumer>;
  /// `on-demand` | `scheduled`.
  rotationPolicy: string;
  /// When the value last changed; null = no rotation recorded.
  rotatedAt: string | null;
  notes: string;
}>;

export function parseCredentials(raw: unknown): ReadonlyArray<Credential> {
  if (!Array.isArray(raw)) throw new Error('the registry answered something that is not a list');
  return raw
    .map(rec)
    .filter((r): r is Record<string, unknown> => r !== null)
    .map((r) => ({
      id: str(r.id),
      kind: str(r.kind),
      issuer: str(r.issuer),
      principal: str(r.principal),
      scopes: Array.isArray(r.scopes) ? r.scopes.filter((s): s is string => typeof s === 'string' && s !== '') : [],
      storageLocation: str(r.storage_location),
      consumers: Array.isArray(r.consumers)
        ? r.consumers
            .map(rec)
            .filter((k): k is Record<string, unknown> => k !== null)
            .map((k) => ({ kind: str(k.kind), location: str(k.location) }))
        : [],
      rotationPolicy: str(r.rotation_policy),
      rotatedAt: typeof r.rotated_at === 'string' && r.rotated_at !== '' ? r.rotated_at : null,
      notes: str(r.notes),
    }))
    .filter((c) => c.id !== '');
}

export function fetchCredentials(): Promise<Settled<ReadonlyArray<Credential>>> {
  return fetchRemote('/api/credentials', parseCredentials);
}

// ---------------------------------------------------------------------
// Provenance
// ---------------------------------------------------------------------

/// The Secret holding the broker's root material — "THE ROOT CREDENTIAL
/// is Secret boss/boss-credential-broker-root", minted by David
/// (infra/cluster/manifests/boss-credential-broker.yaml; pinned there by
/// credentials.test.ts).
export const BROKER_ROOT_SECRET = 'boss-credential-broker-root';

/// The handler family the credential broker runs under.
const BROKER_HANDLER = 'credential.rotate.';

/// An active dispatcher rule that runs the broker, reduced to what
/// provenance needs: its name, its broker handlers, and the text a
/// credential id can be named in (its `when` and those handlers' args).
export type BrokerRule = Readonly<{ name: string; handlers: ReadonlyArray<string>; names: string }>;

export function parseBrokerRules(raw: unknown): ReadonlyArray<BrokerRule> {
  const body = rec(raw);
  if (body && typeof body.error === 'string' && body.error !== '') throw new Error(body.error);
  if (!body || !Array.isArray(body.rules)) throw new Error('the dispatcher answered no rule list');
  return body.rules
    .map(rec)
    // Active rows only, as the header promises: a retired rule rotates
    // nothing, and a row that does not state its status is not taken as
    // active (scope review of 39747321).
    .filter((r): r is Record<string, unknown> => r !== null && r.status === 'active')
    .map((r) => {
      const broker = (Array.isArray(r.do) ? r.do : [])
        .map(rec)
        .filter((d): d is Record<string, unknown> => d !== null && str(d.handler).startsWith(BROKER_HANDLER));
      return {
        name: str(r.name),
        handlers: [...new Set(broker.map((d) => str(d.handler)))],
        names: [str(r.when), ...broker.flatMap((d) => Object.values(rec(d.args) ?? {}).map(str))].join('\n'),
      };
    })
    .filter((r) => r.handlers.length > 0);
}

export function fetchBrokerRules(): Promise<Settled<ReadonlyArray<BrokerRule>>> {
  return fetchRemote('/api/dispatcher/rules', parseBrokerRules);
}

export type Provenance =
  | { kind: 'root' }
  | { kind: 'broker'; handlers: ReadonlyArray<string>; rules: ReadonlyArray<string> }
  | { kind: 'hand' }
  | { kind: 'unknown'; why: string };

export function provenanceOf(c: Credential, rules: Settled<ReadonlyArray<BrokerRule>>): Provenance {
  if (c.storageLocation.includes(BROKER_ROOT_SECRET)) return { kind: 'root' };
  if (rules.kind === 'failed') return { kind: 'unknown', why: rules.error };
  // A rule names a credential the way its `when` spells a Subject: the
  // id as a quoted literal. Whole, so `host-checkout-token` is not
  // `forge-host-checkout-token`.
  const naming = rules.data.filter((r) => r.names.includes(`"${c.id}"`));
  if (naming.length === 0) return { kind: 'hand' };
  return {
    kind: 'broker',
    handlers: [...new Set(naming.flatMap((r) => r.handlers))],
    rules: naming.map((r) => r.name),
  };
}

// ---------------------------------------------------------------------
// Cells and gaps
// ---------------------------------------------------------------------

export type Cell = Readonly<{ text: string; gap: boolean }>;

export function scopesText(c: Credential): Cell {
  return c.scopes.length === 0
    ? { text: 'unverified — the audit fills this', gap: true }
    : { text: c.scopes.join(', '), gap: false };
}

export function lastRotationText(c: Credential): Cell {
  return c.rotatedAt === null
    ? { text: 'never recorded', gap: true }
    : { text: `${c.rotatedAt.slice(0, 10)} ${c.rotatedAt.slice(11, 16)}Z`, gap: false };
}

/// The registry declares a policy, not an interval, so an on-demand row
/// has no due date BY POLICY — said, not drawn as missing — while a
/// scheduled row with no interval to schedule by is a gap.
export function nextDueText(c: Credential): Cell {
  if (c.rotationPolicy === 'on-demand') {
    return { text: 'on demand — no date is due until a rotation is filed', gap: false };
  }
  if (c.rotationPolicy === 'scheduled') {
    return { text: 'scheduled, but the registry declares no interval', gap: true };
  }
  return { text: `policy "${c.rotationPolicy}" is not one the registry knows`, gap: true };
}

/// Every gap on a row, in the order the card draws them.
export function gapsOf(c: Credential): ReadonlyArray<string> {
  return [
    ...(c.consumers.length === 0 ? ['no consumer declared'] : []),
    ...(lastRotationText(c).gap ? ['no rotation recorded'] : []),
    ...(scopesText(c).gap ? ['scopes unverified'] : []),
    ...(c.rotationPolicy === 'scheduled' ? ['no interval for a scheduled rotation'] : []),
    ...(nextDueText(c).gap && c.rotationPolicy !== 'scheduled' ? ['rotation policy unknown'] : []),
  ];
}

// ---------------------------------------------------------------------
// The lifecycle, as packets
// ---------------------------------------------------------------------

export const REQUESTED_FROM = '/it/registry/credentials';
export const ROTATE_KIND = 'rotate-a-credential';
const ITEM_KIND = 'backlog-item';

export type JobBody = Readonly<{
  kind: string;
  title: string;
  tags: ReadonlyArray<string>;
  subject: Readonly<{ id: string; subject_kind: 'custom' }>;
  owner_id: string;
  status: 'open';
  priority: 'standard';
  metadata: Readonly<Record<string, unknown>>;
}>;

/// `boss job file`'s envelope (boss-cli job.rs) — no `opened_on`, so the
/// server clocks the packet and stamps `metadata.opened_at`.
const envelope = (kind: string, title: string, subject: string, ownerId: string, metadata: Record<string, unknown>): JobBody => ({
  kind,
  title,
  tags: [],
  subject: { id: subject, subject_kind: 'custom' },
  owner_id: ownerId,
  status: 'open',
  priority: 'standard',
  metadata,
});

/// Rotate: the rotate-a-credential protocol, opened on the credential's
/// registry id — the Subject the broker's rules match. Its `scope` step
/// is the human's; a broker credential's remaining phases are the
/// machine's, anyone else's are done by hand on the same packet.
export function rotateBody(id: string, ownerId: string): JobBody {
  return envelope(ROTATE_KIND, `Rotate ${id}`, id, ownerId, { credential: id, requested_from: REQUESTED_FROM });
}

/// Retire: no protocol retires a credential and the registry has no
/// retire door, so the packet is a backlog item on the credential's id —
/// triage routes the work, and the item says what the work is.
export function retireBody(c: Credential, ownerId: string): JobBody {
  const consumers =
    c.consumers.length === 0
      ? 'no consumer is declared, so confirm none reads it before revoking'
      : `remove it from every consumer (${c.consumers.map((k) => k.location).join('; ')})`;
  return envelope(ITEM_KIND, `Retire the credential ${c.id}`, c.id, ownerId, {
    area: 'credentials',
    input_channel: 'roadmap',
    credential: c.id,
    requested_from: REQUESTED_FROM,
    description:
      `Retire the credential ${c.id}: revoke it at its issuer (${c.issuer}), ${consumers}, ` +
      `and remove its registry row. No protocol retires a credential yet and the registry has no retire door, ` +
      `so this item routes the work through triage. Filed from ${REQUESTED_FROM}, which holds no value and reads none.`,
  });
}

/// Declare: a row is declared by the instance — the tenant's
/// `seeds/credentials.toml`, published by `boss tenant publish` through
/// `POST /api/credentials/batch`, which no browser reaches — so the
/// packet is a backlog item naming the row to declare.
export function declareBody(id: string, ownerId: string): JobBody {
  return envelope(ITEM_KIND, `Declare the credential ${id} in the registry`, id, ownerId, {
    area: 'credentials',
    input_channel: 'roadmap',
    credential: id,
    requested_from: REQUESTED_FROM,
    description:
      `Declare the credential ${id} in the credentials registry: kind, issuer, principal, scopes, storage location, ` +
      `consumers and rotation policy, in the tenant's seeds/credentials.toml, published by boss tenant publish ` +
      `(POST /api/credentials/batch). A row carries locations, never a value.`,
  });
}

/// Longest word a credential id is made of on the live registry is 11
/// characters (`credentials`); a 40-hex forge token is one 40-character
/// word, kebab-valid, and exactly what must never be typed here. A run of
/// more than 24 non-hyphen characters, in ANY charset, is value-shaped.
const VALUE_SHAPED = /[^-]{25,}/;

/// Why a typed id cannot be declared, or null. The value-shape check runs
/// FIRST and over any charset, and no refusal repeats what was typed: the
/// kebab refusal once echoed the input, so a pasted token that was not
/// kebab-case (mixed case, `_`, `+`, `/`) was drawn in the DOM (scope
/// review of 39747321). The kebab-case rule is boss-jobs
/// `validate_credential`'s, so the item names an id the batch door will
/// accept.
export function declareIdProblem(id: string, existing: ReadonlyArray<string>): string | null {
  if (id === '') return 'Name the credential id to declare.';
  if (VALUE_SHAPED.test(id)) {
    return 'That looks like a value, not a name. This page never takes a credential value — clear the field.';
  }
  if (!/^[a-z0-9-]+$/.test(id)) return 'The id is not kebab-case (lowercase, digits, hyphens) — the registry refuses it.';
  if (existing.includes(id)) return 'That id is already declared.';
  return null;
}

/// POST the packet; the created id, or a thrown error naming the status
/// and body — the caller renders it, never swallows it.
export async function fileJob(body: JobBody): Promise<string> {
  const r = await fetch('/api/jobs', {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(body),
  });
  if (!r.ok) throw new Error(`file ${body.kind}: HTTP ${r.status}: ${await r.text()}`);
  const created = rec(await r.json());
  const id = created ? str(created.id) : '';
  if (id === '') throw new Error(`file ${body.kind}: the create returned no id — refusing to call that filed`);
  return id;
}
