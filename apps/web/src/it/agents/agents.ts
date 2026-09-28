// The Agents tab's read model — the agents registry as a directory
// (backlog 62988516).
//
// David, 2026-09-26, answering 6a123f1f ("agents on the People
// roster?"): "No, Agents can go under a page in the IT department." So
// an agent is never a People row and never headcount, and this is where
// the roster of agents is read. It is the DIRECTORY; the Crew Board (the
// Department Map's shop-floor station) is live activity, and stays so.
//
// READ-ONLY. Writes to the registry stay where they are: `boss tenant
// publish` through `POST /api/agents/batch`, which the gateway does not
// route to a browser at all.
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
import { parseRunRecords, type RunRecord } from '../crew/crew';

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
  /// A Class code under `(employee, department)`, or null.
  department: string | null;
  /// The model a run uses when it does not say, as the rate card spells it.
  defaultModel: string | null;
  hourlyBudgetUsdMicros: number | null;
  maxConcurrentRuns: number | null;
}>;

export function parseAgents(raw: unknown): ReadonlyArray<Agent> {
  return rows(raw)
    .map((r) => ({
      id: str(r.id) ?? '',
      name: str(r.display_name) ?? str(r.id) ?? '',
      aliases: Array.isArray(r.aliases)
        ? r.aliases.filter((a): a is string => typeof a === 'string' && a !== '')
        : [],
      role: str(r.role),
      department: str(r.department),
      defaultModel: str(r.default_model),
      hourlyBudgetUsdMicros: num(r.hourly_budget_usd_micros),
      maxConcurrentRuns: num(r.max_concurrent_runs),
    }))
    .filter((a) => a.id !== '');
}

export const NOT_DECLARED = 'not declared';

export function budgetText(micros: number | null): string {
  return micros === null ? NOT_DECLARED : `$${(micros / 1_000_000).toFixed(2)} an hour`;
}

export function capText(n: number | null): string {
  return n === null ? NOT_DECLARED : String(n);
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
