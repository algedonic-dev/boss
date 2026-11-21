// The Agents tab's read model — the agents registry as a directory
// (backlog 62988516).
//
// David, 2026-09-26, answering 6a123f1f ("agents on the People
// roster?"): "No, Agents can go under a page in the IT department." So
// an agent is never a People row and never headcount, and this is where
// the roster of agents is read. It is the DIRECTORY; live activity is
// the Department Map's shop-floor station.
//
// ONE WRITE: AN AGENT'S SEATS (backlog cdbebc22, David 2026-11-19: "I
// should place you at the root station formally" — from the website,
// where he controls). `seatAgent` below changes one row's `stations` and
// nothing else, through the registry's seat door, `PUT
// /api/agents/{id}/stations`, which writes that column alone and only
// while the registry still holds the seats this page showed (review
// 1a49c3a7: the first cut took the whole row back through the tenant
// batch, and so restored a role another writer had narrowed in between).
// Its authority check is not restated or relaxed here: the door admits
// an operator-tier caller only (boss-jobs `trust::is_trusted`), and a
// browser session reaches that tier one way, the platform owner's
// operator passkey ("Operator access with your passkey", boss-gateway
// `elevation.rs`). So an agent cannot seat itself here: it holds no
// session the gateway would elevate. A change records
// `jobs.agent.updated` naming the seats and the session's actor as
// `updated_by`.
//
// WHAT IS SERVED, AND WHAT IS NOT. Every field below is a column of the
// `agents` table as `GET /api/agents` reads it back (boss-jobs
// `AgentRow`). The packet asked for "the human it reports to", and the
// registry holds no such fact — its declaration refuses a `manager_id`
// by name (`a_field_the_registry_cannot_hold_is_refused_by_name`) — so
// nothing here draws one. The effort an agent runs at is not a registry
// field either: it is read off the agent's own finished runs
// (`agent_runs.detail.effort`), and the page says it is the window's.
//
// ABSENCE IS NEVER A VALUE. A cap the registry holds as NULL is "not
// declared", not zero; a read that fails is `failed`, not an empty list
// — the rule `data/remote.ts` exists for.

import { fetchRemote, type Remote } from '../../data/remote';

const str = (v: unknown): string | null => (typeof v === 'string' && v !== '' ? v : null);
const num = (v: unknown): number | null =>
  typeof v === 'number' && Number.isFinite(v) ? v : null;

/// `/api/agents` answers `{data, total}`; accept a bare array too.
const rows = (raw: unknown): ReadonlyArray<Record<string, unknown>> => {
  if (Array.isArray(raw)) return raw as ReadonlyArray<Record<string, unknown>>;
  const data = (raw as { data?: unknown } | null)?.data;
  return Array.isArray(data) ? (data as ReadonlyArray<Record<string, unknown>>) : [];
};

/// One registry row.
export type Agent = Readonly<{
  /// `agent-<slug>` — what steps, events and runs carry.
  id: string;
  name: string;
  /// The logins that sign as this id (`actor_aliases`), sorted.
  aliases: ReadonlyArray<string>;
  /// A Class code under `(employee, role)`, or null: holds no role.
  role: string | null;
  /// A departments-registry code (`GET /api/departments`; an
  /// `(employee, department)` Class until c87e3d6d), or null.
  department: string | null;
  /// The model a run uses when it does not say, as the rate card spells it.
  defaultModel: string | null;
  /// The model CLASS the row records — the `<class>` of an active
  /// `robots:<class>` pool, what a `model:<class>` step admits it by
  /// (backlog 06d8988b) — or null: records none, never read off the model.
  modelClass: string | null;
  hourlyBudgetUsdMicros: number | null;
  // No concurrency cap: the row's `max_concurrent_runs` bounds nothing
  // since backlog 3e927996 — a robot is bounded by its `robots:<model>`
  // pool alone — so the page does not show it as a limit.
  /// The stations it is seated at, in the order it serves them (design
  /// 506e0a1c); `[]` is seated nowhere, as is a registry predating the key.
  stations: ReadonlyArray<string>;
}>;

const strings = (v: unknown): ReadonlyArray<string> =>
  Array.isArray(v) ? v.filter((s): s is string => typeof s === 'string' && s !== '') : [];

export function parseAgents(raw: unknown): ReadonlyArray<Agent> {
  return rows(raw)
    .map((r) => ({
      id: str(r.id) ?? '',
      name: str(r.display_name) ?? str(r.id) ?? '',
      aliases: strings(r.aliases),
      role: str(r.role),
      department: str(r.department),
      defaultModel: str(r.default_model),
      modelClass: str(r.model_class),
      hourlyBudgetUsdMicros: num(r.hourly_budget_usd_micros),
      stations: strings(r.stations),
    }))
    .filter((a) => a.id !== '');
}

// ---------------------------------------------------------------------
// Seating an agent — the page's one write (backlog cdbebc22).
// ---------------------------------------------------------------------

/// The seats a person typed, in order: commas or whitespace separate
/// them, blanks are dropped. A repeat is KEPT, so the door refuses it by
/// name rather than this page quietly fixing what was typed.
export function parseSeats(text: string): ReadonlyArray<string> {
  return text.split(/[\s,]+/).filter((s) => s !== '');
}

/// Seats in serving order, or that there are none — an empty list said
/// in words, never drawn as a blank.
export function seatsText(stations: ReadonlyArray<string>): string {
  return stations.length === 0 ? 'seated nowhere' : stations.join(' → ');
}

/// One field a write changed (`boss_core::publish::FieldChange`).
export type FieldChange = Readonly<{ field: string; from: unknown; to: unknown }>;

/// A change as the publish line spells it: `stations: ["build"] → ["algedonic"]`.
export function changeText(c: FieldChange): string {
  const show = (v: unknown): string => (typeof v === 'string' ? v : JSON.stringify(v));
  return `${c.field}: ${show(c.from)} → ${show(c.to)}`;
}

export type SeatOutcome =
  /** The door (or the gateway in front of it) said no — a 4xx; nothing landed. */
  | { kind: 'refused'; status: number; why: string }
  /** The door's compare-and-set refused it (409): the seats moved since
   *  this page read them, and nothing was written. `held` is the row read
   *  again, so the next decision is made on what the registry holds. */
  | { kind: 'stale'; why: string; held: Settled<Agent> }
  /** It may or may not have landed: no answer, a server failure (5xx), or
   *  a 2xx whose body is not a seat receipt (review 1a49c3a7 finding 2).
   *  `held` is the row read again AFTER — what the registry holds now,
   *  beside the unknown, never a statement of what this write did. */
  | { kind: 'unknown'; why: string; held: Settled<Agent> }
  /** The door seated it: the row before and after, both as the door's own
   *  transaction read them, and the change (none: it already held these
   *  seats, and nothing was written or recorded). */
  | {
      kind: 'applied';
      before: Agent;
      after: Agent;
      changes: ReadonlyArray<FieldChange>;
    };

/// What a refusal says when the door's body is empty — its 403 carries no
/// text, so the page names what that status means at THIS door.
function refusalText(status: number, body: string): string {
  const said = body.trim();
  if (said !== '') return said;
  if (status === 401) return 'no session: sign in again';
  if (status === 403) {
    return (
      'the agents registry admits only an operator-tier caller, and a browser session reaches ' +
      'the operator tier only through the platform owner’s operator passkey ' +
      '(Operator access with your passkey, on your Me page)'
    );
  }
  return `HTTP ${status}`;
}

const rowOf = (raw: unknown, id: string): Readonly<Record<string, unknown>> | null =>
  rows(raw).find((r) => r.id === id) ?? null;

const isRecord = (v: unknown): v is Readonly<Record<string, unknown>> =>
  typeof v === 'object' && v !== null && !Array.isArray(v);

/// Seats exactly as a receipt states them: a list whose every element is
/// a non-empty string, or nothing. Never filtered or coerced — the
/// directory's `strings` drops what it cannot read, which for a receipt
/// turned `[null, 17]` into "seated nowhere" (review 155c150f P2). An
/// empty list IS a statement and passes.
const exactSeats = (v: unknown): v is ReadonlyArray<string> =>
  Array.isArray(v) && v.every((s) => typeof s === 'string' && s !== '');

/// One row of a seat receipt: `id`'s, and carrying its seats as an exact
/// list — a row that serves no `stations`, or any seat that is not a
/// station id, would parse as fewer seats than it holds, which is a claim
/// the receipt did not make.
const receiptRow = (v: unknown, id: string): Agent | null =>
  isRecord(v) && v.id === id && exactSeats(v.stations) ? (parseAgents([v])[0] ?? null) : null;

/// The seat door's receipt — `{id, before, after, changes}`, both rows
/// `id`'s and every change naming a field — or null when the answer is
/// not one. Null is never read as "no changes" (review 1a49c3a7 finding
/// 2): an empty list is a claim the door makes, not a default.
export function parseSeatReceipt(
  raw: unknown,
  id: string,
): Readonly<{ before: Agent; after: Agent; changes: ReadonlyArray<FieldChange> }> | null {
  if (!isRecord(raw)) return null;
  const before = receiptRow(raw.before, id);
  const after = receiptRow(raw.after, id);
  const changes = raw.changes;
  if (!before || !after || !Array.isArray(changes)) return null;
  if (!changes.every((c) => isRecord(c) && typeof c.field === 'string')) return null;
  return {
    before,
    after,
    changes: (changes as ReadonlyArray<Record<string, unknown>>).map((c) => ({
      field: String(c.field),
      from: c.from,
      to: c.to,
    })),
  };
}

/// The row as the registry holds it now, read after a write whose outcome
/// the door's answer did not settle.
async function heldNow(id: string): Promise<Settled<Agent>> {
  const read = await fetchRemote('/api/agents', (raw) => {
    const row = rowOf(raw, id);
    return row ? (parseAgents([row])[0] ?? null) : null;
  });
  if (read.kind === 'failed') return read;
  return read.data === null
    ? { kind: 'failed', error: `the registry does not list ${id}` }
    : { kind: 'ready', data: read.data };
}

/// Seat `id` at `stations`, in order, through the registry's seat door,
/// `PUT /api/agents/{id}/stations` (review 1a49c3a7 finding 1). The door
/// writes the seats and NO other field, and only while the registry still
/// holds `expected` — the seats this page showed when the person decided.
/// The first cut re-read the row and took the WHOLE row back through the
/// batch, so a role another writer narrowed between that read and the
/// write was restored by a seat edit; a fresher read only shortened that
/// window. "Before" and "after" are the door's own transaction's rows.
export async function seatAgent(
  id: string,
  expected: ReadonlyArray<string>,
  stations: ReadonlyArray<string>,
): Promise<SeatOutcome> {
  let answer: Response;
  try {
    answer = await fetch(`/api/agents/${encodeURIComponent(id)}/stations`, {
      method: 'PUT',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ expected: [...expected], stations: [...stations] }),
    });
  } catch (e) {
    const detail = e instanceof Error ? e.message : String(e);
    return { kind: 'unknown', why: `no answer: ${detail}`, held: await heldNow(id) };
  }
  if (!answer.ok) {
    const body = (await answer.text().catch(() => '')).trim();
    if (answer.status >= 500) {
      const why = `HTTP ${answer.status}${body === '' ? '' : `: ${body}`}`;
      return { kind: 'unknown', why, held: await heldNow(id) };
    }
    if (answer.status === 409) {
      const why = body === '' ? 'the seats changed since this page read them' : body;
      return { kind: 'stale', why, held: await heldNow(id) };
    }
    return { kind: 'refused', status: answer.status, why: refusalText(answer.status, body) };
  }
  const receipt = parseSeatReceipt(await answer.json().catch(() => null), id);
  if (receipt === null) {
    return {
      kind: 'unknown',
      why: `HTTP ${answer.status}, but the answer is not the seat door's receipt, so what it did is not known`,
      held: await heldNow(id),
    };
  }
  return { kind: 'applied', ...receipt };
}

// ---------------------------------------------------------------------
// The finish record — `GET /api/agent-runs` (backlog 5082a08b). Moved
// here from the retired Crew Board's read model (backlog 462fb22a), whose
// only live readers were this page's.
// ---------------------------------------------------------------------

/// One row of `agent_runs`: a run that reached a terminal, with what it
/// cost. `boss dispatch --report` writes it, keyed by the agent-run
/// packet's id, and ONLY at a terminal — the row is insert-once
/// (8f1de7bf) — so an OPEN run has no cost to show yet, and this is a
/// list of finished runs rather than a column on the open ones.
///
/// Two limits of the record ride on the rows rather than being papered
/// over: rows before the effort instrumentation carry no `effort`
/// (fd5ce137's era boundary), and rows before 65c9c05a carry no branch.
/// Each renders as not recorded, never as a guess.
export type RunRecord = Readonly<{
  runId: string;
  actor: string | null;
  model: string | null;
  outcome: string | null;
  finishedAt: string | null;
  /// Wall-clock minutes, start to finish; `null` without both stamps.
  minutes: number | null;
  tokens: number | null;
  /// Priced cost in micro-dollars; `null` is UNPRICED, never free.
  usdMicros: number | null;
  branch: string | null;
  packet: string | null;
  effort: string | null;
}>;

export function parseRunRecords(raw: unknown): ReadonlyArray<RunRecord> {
  return rows(raw)
    .map((r) => {
      const detail = (r.detail && typeof r.detail === 'object' ? r.detail : {}) as Record<
        string,
        unknown
      >;
      const started = Date.parse(str(r.started_at) ?? '');
      const finished = Date.parse(str(r.finished_at) ?? '');
      return {
        runId: str(r.run_id) ?? '',
        actor: str(r.actor_id),
        model: str(r.model),
        outcome: str(r.outcome),
        finishedAt: str(r.finished_at),
        minutes:
          Number.isFinite(started) && Number.isFinite(finished)
            ? Math.round((finished - started) / 60_000)
            : null,
        tokens: num(r.total_tokens),
        usdMicros: num(r.usd_micros),
        branch: str(r.branch),
        packet: str(r.job_id),
        effort: str(detail.effort),
      };
    })
    .filter((r) => r.runId !== '');
}

/// A run's cost as the page prints it. Unpriced is said in words: a
/// `$0.00` for a run nobody priced would read as a free one.
export function costText(r: RunRecord): string {
  return r.usdMicros === null ? 'not priced' : `$${(r.usdMicros / 1_000_000).toFixed(2)}`;
}

export const NOT_DECLARED = 'not declared';

export function budgetText(micros: number | null): string {
  return micros === null ? NOT_DECLARED : `$${(micros / 1_000_000).toFixed(2)} an hour`;
}

/// The efforts an agent's runs in the window ran at, most-run first:
/// `high ×18, medium ×2`. A run before the effort instrumentation
/// carries none, and is counted as not recorded rather than guessed.
export function effortsRun(runs: ReadonlyArray<RunRecord>): string {
  const counts = runs.reduce<ReadonlyMap<string, number>>(
    (m, r) => new Map(m).set(r.effort ?? 'not recorded', (m.get(r.effort ?? 'not recorded') ?? 0) + 1),
    new Map(),
  );
  return [...counts.entries()]
    .sort((a, b) => b[1] - a[1] || a[0].localeCompare(b[0]))
    .map(([effort, n]) => `${effort} ×${n}`)
    .join(', ');
}

/// Open steps an agent holds: distinct steps across its id and every
/// alias, and whether any read reached its limit — then the count is a
/// floor, and says so (a limit is not a filter).
export type Held = Readonly<{ steps: number; capped: boolean }>;

export function heldText(h: Held): string {
  return h.capped ? `at least ${h.steps}` : String(h.steps);
}

/// Finished runs read per agent, newest first. `/api/agent-runs` serves
/// no total, so a full window is said as "the newest N".
export const RUN_WINDOW = 20;
/// The assignments read's own ceiling (boss-jobs `MAX_LIMIT`).
export const HELD_LIMIT = 1000;

export type Settled<T> = Exclude<Remote<T>, { kind: 'loading' }>;

export type AgentDetail = Readonly<{
  runs: Settled<ReadonlyArray<RunRecord>>;
  held: Settled<Held>;
}>;

export function fetchAgents(): Promise<Settled<ReadonlyArray<Agent>>> {
  return fetchRemote('/api/agents', parseAgents);
}

const stepIds = (raw: unknown): ReadonlyArray<string> =>
  rows(raw)
    .map((r) => str((r.step as Record<string, unknown> | undefined)?.id))
    .filter((id): id is string => id !== null);

/// One agent's runs and held steps. Runs are read by the registered id
/// alone: `agent_runs.actor_id` carries it since design 6fda05ae. Held
/// steps are read under the id AND each alias, because a step assigned
/// by login before the door resolved logins still carries the address
/// (measured 2026-09-27: 430 under `agent-claude`, 6 under
/// `claude@algedonic.dev`).
export async function fetchAgentDetail(agent: Agent): Promise<AgentDetail> {
  const q = encodeURIComponent;
  const [runs, ...heldReads] = await Promise.all([
    fetchRemote(`/api/agent-runs?actor_id=${q(agent.id)}&limit=${RUN_WINDOW}`, parseRunRecords),
    ...[agent.id, ...agent.aliases].map((who) =>
      fetchRemote(`/api/jobs/assignments?assignee_id=${q(who)}&limit=${HELD_LIMIT}`, stepIds),
    ),
  ]);
  const failed = heldReads.find((r) => r.kind === 'failed');
  const held: Settled<Held> =
    failed && failed.kind === 'failed'
      ? failed
      : {
          kind: 'ready',
          data: {
            steps: new Set(heldReads.flatMap((r) => (r.kind === 'ready' ? r.data : []))).size,
            capped: heldReads.some((r) => r.kind === 'ready' && r.data.length >= HELD_LIMIT),
          },
        };
  return { runs, held };
}
