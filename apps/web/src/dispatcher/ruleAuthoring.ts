// Types + typed API client for the dispatcher rule-authoring surface
// (the control-plane writes behind `boss-dispatcher`'s
// `/api/dispatcher/rules*` endpoints). Mirrors the step-plugins
// authoring fetch + error-handling style; deserialized once at the
// call site per the repo's no-shared-types convention.
//
// The read-only cascade feed types (DispatcherRule, DispatcherRuleDo,
// DispatcherRules) live in ./types — reused here so one rule-content
// shape spans the read and write paths.

import type { DispatcherRule, DispatcherRuleDo, DispatcherRules } from './types';

export type { DispatcherRule, DispatcherRuleDo } from './types';

/** A rule's lifecycle state — matches `dispatcher_rules.status`. */
export type RuleStatus = 'draft' | 'active' | 'retired';

/** One stored `dispatcher_rules` row — the rule content plus its
 *  lifecycle. Mirror of boss_dispatcher::rules::authoring::RuleVersion
 *  (which serializes `do_steps`→`do`, `when_expr`→`when`). */
export type RuleVersion = Readonly<{
  name: string;
  version: number;
  status: RuleStatus;
  on_event: string;
  when: string | null;
  do: ReadonlyArray<DispatcherRuleDo>;
  delay: string | null;
  created_at: string;
}>;

/** Request body for create-draft / validate — the editable rule spec.
 *  `name` keys the rule; the server assigns the version. */
export type RuleSpec = Readonly<{
  name: string;
  on_event: string;
  when?: string | null;
  do: ReadonlyArray<DispatcherRuleDo>;
  delay?: string | null;
}>;

/** The editor's raw form model — the editable fields before
 *  normalization. `do` args are key/value rows (typed end-to-end). */
export type RuleForm = Readonly<{
  name: string;
  on_event: string;
  when: string;
  delay: string;
  do: ReadonlyArray<{
    handler: string;
    args: ReadonlyArray<{ key: string; value: string }>;
  }>;
}>;

/** Normalize an editor form into the API spec: trim everything, drop steps
 *  with an empty handler, drop args with an empty key, and null out an
 *  empty `when`/`delay`. Pure — the editor's Validate + Save both go through
 *  it, so what you validate is exactly what you save. */
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
  return {
    name: form.name.trim(),
    on_event: form.on_event.trim(),
    when: form.when.trim().length > 0 ? form.when.trim() : null,
    do: doSteps,
    delay: form.delay.trim().length > 0 ? form.delay.trim() : null,
  };
}

/** `POST /rules/_validate` result — a dry-run parse outcome. */
export type ValidateResult = Readonly<{ ok: boolean; error: string | null }>;

async function ensureOk(r: Response): Promise<Response> {
  if (!r.ok) throw new Error(`HTTP ${r.status}: ${await r.text()}`);
  return r;
}

/** The ACTIVE rules — the same feed the cascade viz reads. */
export async function listActiveRules(): Promise<ReadonlyArray<DispatcherRule>> {
  const r = await ensureOk(await fetch('/api/dispatcher/rules'));
  const payload = (await r.json()) as DispatcherRules;
  if (payload.error) throw new Error(payload.error);
  return payload.rules;
}

/** All versions of one rule, oldest first (draft + active + retired). */
export async function listVersions(name: string): Promise<ReadonlyArray<RuleVersion>> {
  const r = await ensureOk(
    await fetch(`/api/dispatcher/rules/${encodeURIComponent(name)}/versions`),
  );
  return (await r.json()) as RuleVersion[];
}

/** A specific stored version. */
export async function getVersion(name: string, version: number): Promise<RuleVersion> {
  const r = await ensureOk(
    await fetch(
      `/api/dispatcher/rules/${encodeURIComponent(name)}/versions/${version}`,
    ),
  );
  return (await r.json()) as RuleVersion;
}

/** Append a new draft version (validated server-side; `400` if it
 *  doesn't parse). Returns the stored draft. */
export async function createDraft(spec: RuleSpec): Promise<RuleVersion> {
  const r = await ensureOk(
    await fetch('/api/dispatcher/rules', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify(spec),
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
