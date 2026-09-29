// Types + typed API client for the dispatcher rule-authoring surface
// (the control-plane writes behind `boss-dispatcher`'s
// `/api/dispatcher/rules*` endpoints). Mirrors the step-plugins
// authoring fetch + error-handling style; deserialized once at the
// call site per the repo's no-shared-types convention.
//
// The read-only cascade feed types (DispatcherRule, DispatcherRuleDo,
// DispatcherRules) live in ./types — reused here so one rule-content
// shape spans the read and write paths.

import type {
  AuthoredRegistry,
  DispatcherRule,
  DispatcherRuleDo,
  DispatcherRules,
  DispatcherSchedule,
} from './types';

export type { DispatcherRule, DispatcherRuleDo } from './types';

/** A rule's lifecycle state — matches `dispatcher_rules.status`. */
export type RuleStatus = 'draft' | 'active' | 'retired';

/** One stored `dispatcher_rules` row — the rule content plus its
 *  lifecycle. Mirror of boss_dispatcher::rules::authoring::RuleVersion
 *  (which serializes `do_steps`→`do`, `when_expr`→`when`).
 *
 *  `on_event`, `schedule` and `source` are OMITTED when the server holds
 *  None (`skip_serializing_if`): a schedule rule carries no `on_event`
 *  key at all — 26 of 80 live rules, measured 2026-09-27 (backlog
 *  b38360ed) — and a product row carries no `source`. The three `*_by`
 *  are served as `null` when no door recorded the act (backlog 847af5c7). */
export type RuleVersion = Readonly<{
  name: string;
  version: number;
  status: RuleStatus;
  on_event?: string | null;
  schedule?: DispatcherSchedule | null;
  when: string | null;
  do: ReadonlyArray<DispatcherRuleDo>;
  delay: string | null;
  /** `tenant:<id>` for a tenant's rule; absent for the product's. */
  source?: string | null;
  created_by: string | null;
  published_by: string | null;
  retired_by: string | null;
  created_at: string;
}>;

/** Request body for create-draft / validate — the editable rule spec.
 *  `name` keys the rule; the server assigns the version. Exactly one of
 *  `on_event` / `schedule` is present: the server's RawRule refuses both
 *  and neither (`Rule::from_raw`), and its `deny_unknown_fields` refuses
 *  any key it does not name. */
export type RuleSpec = Readonly<{
  name: string;
  on_event?: string;
  schedule?: DispatcherSchedule;
  when?: string | null;
  do: ReadonlyArray<DispatcherRuleDo>;
  delay?: string | null;
}>;

/** Which trigger the form is editing. */
export type RuleTrigger = 'event' | 'schedule';

/** The editor's raw form model — the editable fields before
 *  normalization. `do` args are key/value rows (typed end-to-end). Both
 *  triggers' fields are held so switching `trigger` loses no typing, and
 *  only the chosen one is sent. */
export type RuleForm = Readonly<{
  name: string;
  trigger: RuleTrigger;
  on_event: string;
  cadence: string;
  anchor_date: string;
  business_calendar: string;
  when: string;
  delay: string;
  do: ReadonlyArray<{
    handler: string;
    args: ReadonlyArray<{ key: string; value: string }>;
  }>;
}>;

/** The form a stored row seeds. Every text field is a string, never
 *  `undefined`: a schedule row serves no `on_event`, and seeding the
 *  editor's `onEvent` with that undefined is what made buildRuleSpec
 *  throw on `.trim()` and wedge Save (backlog b38360ed). */
export function formFromVersion(v: RuleVersion): RuleForm {
  const s = v.schedule ?? null;
  return {
    name: v.name,
    trigger: s ? 'schedule' : 'event',
    on_event: v.on_event ?? '',
    cadence: s?.cadence ?? '',
    anchor_date: s?.anchor_date ?? '',
    business_calendar: s?.business_calendar ?? '',
    when: v.when ?? '',
    delay: v.delay ?? '',
    do: v.do.map((d) => ({
      handler: d.handler,
      args: Object.entries(d.args).map(([key, value]) => ({ key, value })),
    })),
  };
}

/** Normalize an editor form into the API spec: trim everything, drop steps
 *  with an empty handler, drop args with an empty key, null out an empty
 *  `when`/`delay`, and send the chosen trigger ONLY — a topic typed while
 *  the trigger is the schedule is not sent, so a draft cannot silently
 *  turn a schedule rule into an event rule (backlog b38360ed). Pure — the
 *  editor's Validate + Save both go through it, so what you validate is
 *  exactly what you save. */
export function buildRuleSpec(form: RuleForm): RuleSpec {
  const doSteps = form.do
    .filter((row) => row.handler.trim().length > 0)
    .map((row) => {
      const args: Record<string, string> = {};
      for (const { key, value } of row.args) {
        const k = key.trim();
        if (k.length > 0) args[k] = value;
      }
      return { handler: row.handler.trim(), args };
    });
  const calendar = form.business_calendar.trim();
  const trigger =
    form.trigger === 'schedule'
      ? {
          schedule: {
            cadence: form.cadence.trim(),
            anchor_date: form.anchor_date.trim(),
            ...(calendar.length > 0 ? { business_calendar: calendar } : {}),
          },
        }
      : { on_event: form.on_event.trim() };
  return {
    name: form.name.trim(),
    ...trigger,
    when: form.when.trim().length > 0 ? form.when.trim() : null,
    do: doSteps,
    delay: form.delay.trim().length > 0 ? form.delay.trim() : null,
  };
}

/** Why Save should not send this spec, or `null` when it is whole. The
 *  server refuses the same drafts; this names the missing field before
 *  a round trip does. */
export function draftRefusal(spec: RuleSpec): string | null {
  if (spec.name.length === 0) return 'Rule name is required.';
  if (spec.schedule) {
    if (spec.schedule.cadence.length === 0) return 'A schedule needs a cadence.';
    if (spec.schedule.anchor_date.length === 0) return 'A schedule needs an anchor date.';
    return null;
  }
  if ((spec.on_event ?? '').length === 0) return 'on_event is required.';
  return null;
}

/** The `source` a new draft of this rule restates (backlog 636b9868):
 *  the ACTIVE row's, else the newest row still in service (a draft), else
 *  `null`. The draft door refuses a draft whose source differs from the
 *  name's non-retired owner, so a draft of a tenant's rule must carry
 *  `tenant:<id>`; a name whose rows are all retired is nobody's. */
export function liveSource(versions: ReadonlyArray<RuleVersion>): string | null {
  const active = versions.find((v) => v.status === 'active');
  const live = active ?? [...versions].reverse().find((v) => v.status !== 'retired');
  return live?.source ?? null;
}

/** `POST /rules/_validate` result — a dry-run parse outcome. */
export type ValidateResult = Readonly<{ ok: boolean; error: string | null }>;

async function ensureOk(r: Response): Promise<Response> {
  if (!r.ok) throw new Error(`HTTP ${r.status}: ${await r.text()}`);
  return r;
}

/** The ACTIVE rules, and where their whys were read from. */
export type ActiveRules = Readonly<{
  rules: ReadonlyArray<DispatcherRule>;
  /** `null` when the dispatcher served no block (an older build) — read
   *  it as "could not tell", never as a healthy registry. */
  authoredRegistry: AuthoredRegistry | null;
  /** Handler name → the kinds it causes to be emitted: this build's
   *  roster, served beside the rules (backlog 0034d5ef). */
  handlerEmits: Readonly<Record<string, ReadonlyArray<string>>>;
}>;

/** The ACTIVE rules — the same feed the cascade viz reads. The
 *  `authored_registry` block rides along (backlog f9e34a2c): without it
 *  every row's `authored: false` under an unset or unreadable
 *  BOSS_DISPATCHER_RULES reads as drift. */
export async function listActiveRules(): Promise<ActiveRules> {
  const r = await ensureOk(await fetch('/api/dispatcher/rules'));
  const payload = (await r.json()) as DispatcherRules;
  if (payload.error) throw new Error(payload.error);
  return {
    rules: payload.rules,
    authoredRegistry: payload.authored_registry ?? null,
    handlerEmits: payload.handler_emits ?? {},
  };
}

/** All versions of one rule, oldest first (draft + active + retired).
 *  Each carries its whole content, which is why the editor's history
 *  expands a row from this read and has no per-version fetch (the
 *  `getVersion` it had was never called, backlog 9550ac95). */
export async function listVersions(name: string): Promise<ReadonlyArray<RuleVersion>> {
  const r = await ensureOk(
    await fetch(`/api/dispatcher/rules/${encodeURIComponent(name)}/versions`),
  );
  return (await r.json()) as RuleVersion[];
}

/** Append a new draft version (validated server-side; `400` if it
 *  doesn't parse). Returns the stored draft. `source` restates who
 *  declares the rule ([`liveSource`]); `null` sends no key, which the
 *  door reads as the product's. Only the draft door takes it —
 *  `_validate` parses a bare RawRule and refuses the key. */
export async function createDraft(spec: RuleSpec, source: string | null): Promise<RuleVersion> {
  const body = source === null ? spec : { ...spec, source };
  const r = await ensureOk(
    await fetch('/api/dispatcher/rules', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify(body),
    }),
  );
  return (await r.json()) as RuleVersion;
}

/** Dry-run a draft without persisting — for the live "Validate"
 *  affordance. Does NOT throw on a parse error; the parse outcome is
 *  the `{ ok, error }` body. */
export async function validateRule(spec: RuleSpec): Promise<ValidateResult> {
  const r = await ensureOk(
    await fetch('/api/dispatcher/rules/_validate', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify(spec),
    }),
  );
  return (await r.json()) as ValidateResult;
}

/** Activate the latest draft, retiring the prior active version. */
export async function publishRule(name: string): Promise<RuleVersion> {
  const r = await ensureOk(
    await fetch(`/api/dispatcher/rules/${encodeURIComponent(name)}/publish`, {
      method: 'POST',
    }),
  );
  return (await r.json()) as RuleVersion;
}

/** Retire the active version (`204`; idempotent server-side). */
export async function retireRule(name: string): Promise<void> {
  await ensureOk(
    await fetch(`/api/dispatcher/rules/${encodeURIComponent(name)}/retire`, {
      method: 'POST',
    }),
  );
}
