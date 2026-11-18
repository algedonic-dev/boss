// File an ops request from the verb list, on the Estate page (backlog
// 7aef2405; David 2026-11-07 ~15:05Z: "In general, I just want to be
// using the website UI for my controls").
//
// A rendered plan's page already files its write (jobs/fileTheRequest.ts,
// car 72e7a790), but on 2026-11-07 the gate volume move was still filed
// from the pod shell, line by line: filing the plan, or any verb at all,
// took `boss ops <host> <verb> <args>` in a terminal. This is the form
// that does what that line does — the person picks a verb and a host off
// the allowlist and fills its params — and the jobs API files it as the
// signed-in person: `POST /api/jobs/file-an-ops-request`, whose packet is
// the one `boss ops` files, through the same create door.
//
// NOTHING IS JUDGED HERE. The verb list is the server's
// (`GET /api/jobs/ops-verbs`, the compiled-in allowlist); whether a call
// files — an unlisted verb, a host the verb does not serve, a param
// outside its pattern — is boss_jobs::ops_verbs::validate's, the rules
// `boss ops` applies, and a refusal is shown verbatim. The one thing this
// module decides is how a form's named fields become the positional args
// the allowlist reads (`argsFor`), which is the form's own question.

import { fetchRemote, type Remote } from '../../data/remote';

export type VerbParam = Readonly<{
  name: string;
  pattern: string | null;
  one_of: ReadonlyArray<string> | null;
  default: string | null;
  optional: boolean;
  max: number | null;
}>;

export type OpsVerb = Readonly<{
  name: string;
  about: string;
  hosts: ReadonlyArray<string>;
  /** The params a filer gives — never an approval verb's plan_sha256. */
  params: ReadonlyArray<VerbParam>;
  mutating: boolean;
  requires_approval: boolean;
  requires_review: boolean;
  approvers: ReadonlyArray<string>;
  plan_verb: string | null;
  /** The write this verb is the plan of, when it is a plan-* preview. */
  plan_for: string | null;
}>;

const rec = (v: unknown): Record<string, unknown> | null =>
  v !== null && typeof v === 'object' && !Array.isArray(v) ? (v as Record<string, unknown>) : null;
const str = (v: unknown): string | null => (typeof v === 'string' && v !== '' ? v : null);
const strs = (v: unknown): ReadonlyArray<string> | null =>
  Array.isArray(v) && v.every((x) => typeof x === 'string') ? (v as ReadonlyArray<string>) : null;

function parseParam(v: unknown): VerbParam | null {
  const r = rec(v);
  const name = str(r?.name);
  if (!r || !name) return null;
  return {
    name,
    pattern: str(r.pattern),
    one_of: strs(r.one_of),
    default: typeof r.default === 'string' ? r.default : null,
    optional: r.optional === true,
    max: typeof r.max === 'number' ? r.max : null,
  };
}

function parseVerb(v: unknown): OpsVerb | null {
  const r = rec(v);
  const name = str(r?.name);
  const hosts = strs(r?.hosts);
  const approvers = strs(r?.approvers);
  const rawParams = r?.params;
  if (!r || !name || !hosts || !approvers || !Array.isArray(rawParams)) return null;
  const params = rawParams.map(parseParam);
  if (params.some((p) => p === null)) return null;
  return {
    name,
    about: typeof r.about === 'string' ? r.about : '',
    hosts,
    params: params as ReadonlyArray<VerbParam>,
    mutating: r.mutating === true,
    requires_approval: r.requires_approval === true,
    requires_review: r.requires_review === true,
    approvers,
    plan_verb: str(r.plan_verb),
    plan_for: str(r.plan_for),
  };
}

/** The server's list. A shape this page cannot read THROWS, so the read
 *  lands as `failed` and says so — never as an empty allowlist. */
export function parseOpsVerbs(raw: unknown): ReadonlyArray<OpsVerb> {
  const list = rec(raw)?.verbs;
  if (!Array.isArray(list)) throw new Error('the verb list came back in a shape this page cannot read');
  const verbs = list.map(parseVerb);
  const bad = verbs.findIndex((v) => v === null);
  if (bad >= 0) throw new Error(`verb ${bad + 1} of the list came back in a shape this page cannot read`);
  return verbs as ReadonlyArray<OpsVerb>;
}

export const OPS_VERBS_PATH = '/api/jobs/ops-verbs';
export const FILE_PATH = '/api/jobs/file-an-ops-request';

export function loadOpsVerbs(): Promise<Exclude<Remote<ReadonlyArray<OpsVerb>>, { kind: 'loading' }>> {
  return fetchRemote(OPS_VERBS_PATH, parseOpsVerbs);
}

export type Args = { kind: 'args'; args: ReadonlyArray<string> } | { kind: 'refused'; why: string };

/** The form's named fields as the positional args the allowlist reads.
 *  Trailing blanks are left off, so an omitted optional or defaulted
 *  param takes the verb file's own default on the server. A blank BEFORE
 *  a filled field cannot be left off — the args are positional — so it
 *  takes its default, or the form says which field to fill. */
export function argsFor(verb: OpsVerb, values: Readonly<Record<string, string>>): Args {
  const filled = verb.params.map((p) => (values[p.name] ?? '').trim());
  let last = filled.length - 1;
  while (last >= 0 && filled[last] === '') last -= 1;
  const args: string[] = [];
  for (let i = 0; i <= last; i += 1) {
    const p = verb.params[i]!;
    const v = filled[i]!;
    if (v !== '') args.push(v);
    else if (p.default !== null) args.push(p.default);
    else {
      const later = verb.params[last]!.name;
      return {
        kind: 'refused',
        why: `${p.name} is empty while ${later} is filled; the arguments are positional, so fill ${p.name} or clear every field after it.`,
      };
    }
  }
  return { kind: 'args', args };
}

export type Call = Readonly<{ host: string; verb: string; args: ReadonlyArray<string> }>;
export type Filed = { kind: 'filed'; id: string } | { kind: 'refused'; why: string };

/** POST the call — `{host, verb, args}` and nothing else. The created id,
 *  or the server's refusal verbatim with its status. */
export async function fileAnOpsRequest(call: Call): Promise<Filed> {
  let r: Response;
  try {
    r = await fetch(FILE_PATH, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ host: call.host, verb: call.verb, args: call.args }),
    });
  } catch (e) {
    return { kind: 'refused', why: `file the request: ${e instanceof Error ? e.message : String(e)}` };
  }
  const text = await r.text();
  let body: unknown = null;
  try {
    body = JSON.parse(text);
  } catch {
    body = null;
  }
  if (!r.ok) {
    const why = str(rec(body)?.error) ?? text;
    return { kind: 'refused', why: `file the request: HTTP ${r.status}: ${why}` };
  }
  const id = str(rec(body)?.id);
  return id
    ? { kind: 'filed', id }
    : { kind: 'refused', why: 'file the request: the create returned no id — refusing to call that filed' };
}

/** What a param admits, in a line under its field. */
export function paramHint(p: VerbParam): string {
  const parts = [
    p.one_of ? `one of ${p.one_of.join(', ')}` : p.pattern ? `matches ${p.pattern}` : null,
    p.max !== null ? `at most ${p.max}` : null,
    p.default !== null ? `default ${p.default}` : p.optional ? 'optional' : 'required',
  ];
  return parts.filter((x): x is string => x !== null).join(' · ');
}
