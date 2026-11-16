// A rendered plan files its mutating request from the website (backlog
// 7aef2405, David 2026-11-05: "None of those steps are actually ready
// for me though, right?").
//
// A plan-* preview packet — an ops-request whose verb is another verb's
// `plan_verb` — that the runner answered with exit 0 holds the whole
// request: the write, its host, its args. The jobs API files it,
// `POST /api/jobs/{plan}/file-the-request`, as the signed-in actor,
// through the create door's policy, and the filed request waits on its
// approve step for the verb's approvers' passkey, exactly as one
// `boss ops` files from a terminal. Filing is not approving.
//
// NOTHING IS JUDGED HERE. Whether a packet is a plan, whether it
// rendered clean, which write it files and with which flags, whether one
// is already open: all of it is the server's reading
// (`GET …/file-the-request`, boss_jobs::ops_verbs::read_plan, the rules
// `boss ops` applies), parsed and drawn. The POST sends no body: the
// server refuses one, because no filer supplies the args, the flags or
// `plan_sha256` — the runner appends the SIGNED plan's hash.

import { fetchRemote, type Remote } from '../data/remote';

export type PlanRequest = Readonly<{
  host: string;
  verb: string;
  args: ReadonlyArray<string>;
  title: string;
  mutating: boolean;
  requires_approval: boolean;
  requires_review: boolean;
  approvers: ReadonlyArray<string>;
}>;

export type PlanPreview =
  /** The packet is no plan: nothing to offer, nothing drawn. */
  | { kind: 'not-a-plan' }
  /** A plan whose write cannot be filed now, in the server's words; an
   *  open request filed from it is named when that is the reason. */
  | { kind: 'refused'; write: string | null; why: string; open_request: string | null }
  | { kind: 'fileable'; request: PlanRequest };

const rec = (v: unknown): Record<string, unknown> | null =>
  v !== null && typeof v === 'object' && !Array.isArray(v) ? (v as Record<string, unknown>) : null;
const str = (v: unknown): string | null => (typeof v === 'string' && v !== '' ? v : null);
const strs = (v: unknown): ReadonlyArray<string> | null =>
  Array.isArray(v) && v.every((x) => typeof x === 'string') ? (v as ReadonlyArray<string>) : null;

function parseRequest(v: unknown): PlanRequest | null {
  const r = rec(v);
  if (!r) return null;
  const host = str(r.host);
  const verb = str(r.verb);
  const title = str(r.title);
  const args = strs(r.args);
  const approvers = strs(r.approvers);
  if (!host || !verb || !title || !args || !approvers) return null;
  return {
    host,
    verb,
    args,
    title,
    mutating: r.mutating === true,
    requires_approval: r.requires_approval === true,
    requires_review: r.requires_review === true,
    approvers,
  };
}

/** The server's reading. A shape this page cannot read is a refusal
 *  that says so — never a silent "not a plan". */
export function parsePlanPreview(raw: unknown): PlanPreview {
  const r = rec(raw);
  if (r && r.plan === false) return { kind: 'not-a-plan' };
  if (r && r.plan === true && r.fileable === true) {
    const request = parseRequest(r.request);
    if (request) return { kind: 'fileable', request };
  }
  if (r && r.plan === true && r.fileable === false) {
    return {
      kind: 'refused',
      write: str(r.write),
      why: str(r.why) ?? 'the server gave no reason',
      open_request: str(r.open_request),
    };
  }
  return {
    kind: 'refused',
    write: null,
    why: 'the file-the-request reading came back in a shape this page cannot read',
    open_request: null,
  };
}

export const fileTheRequestPath = (jobId: string): string =>
  `/api/jobs/${encodeURIComponent(jobId)}/file-the-request`;

export function loadPlanPreview(jobId: string): Promise<Exclude<Remote<PlanPreview>, { kind: 'loading' }>> {
  return fetchRemote(fileTheRequestPath(jobId), parsePlanPreview);
}

export type Filed = { kind: 'filed'; id: string } | { kind: 'refused'; why: string };

/** POST with no body. The created id, or the refusal verbatim with its
 *  status — the page renders it, never swallows it. */
export async function fileTheRequest(jobId: string): Promise<Filed> {
  let r: Response;
  try {
    r = await fetch(fileTheRequestPath(jobId), { method: 'POST' });
  } catch (e) {
    return { kind: 'refused', why: `file the request: ${e instanceof Error ? e.message : String(e)}` };
  }
  const text = await r.text();
  if (!r.ok) return { kind: 'refused', why: `file the request: HTTP ${r.status}: ${text}` };
  let id: string | null = null;
  try {
    id = str(rec(JSON.parse(text))?.id);
  } catch {
    id = null;
  }
  return id
    ? { kind: 'filed', id }
    : { kind: 'refused', why: 'file the request: the create returned no id — refusing to call that filed' };
}

/** Whether a packet is one this surface should ask about: it carries
 *  an ops request's `verb` and `host`. Data-keyed (CLAUDE.md §9); the
 *  server says whether that verb is a plan. */
export function carriesAnOpsRequest(metadata: Readonly<Record<string, unknown>> | undefined): boolean {
  return typeof metadata?.verb === 'string' && typeof metadata?.host === 'string';
}

/** Who the filed request waits on, in a sentence — for a plan's write
 *  here, and for a verb picked off the list (it/estate/fileAnOpsRequest). */
export function waitsOn(
  request: Pick<PlanRequest, 'approvers' | 'requires_approval' | 'requires_review'>,
): string {
  const who = request.approvers.length > 0 ? request.approvers.join(', ') : 'nobody named';
  if (!request.requires_approval) return 'It runs once the host picks it up; it needs no approval.';
  const review = request.requires_review ? 'an independent review of the plan, then ' : '';
  return `It waits on ${review}a passkey approval by ${who}.`;
}
