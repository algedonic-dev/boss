// Shared write path for the platform step surfaces (packet cc9d7fc6).
//
// The class this exists to kill: surfaces that `await fetch(...)` a
// PUT and ignore the response. A Complete click that 400s did nothing
// visible — the operator believed the step closed. Every step-surface
// write now flows through here and comes back as a discriminated
// result the surface must branch on: `ok` continues, `failed` renders
// inline and leaves state untouched.

export type StepWriteResult =
  | { kind: 'ok'; response: Response }
  | { kind: 'failed'; error: string };

const MAX_BODY_CHARS = 200;

/// One human-readable line for a refused write. Prefers the server's
/// own words: the `{error|message|detail}` JSON fields the BOSS APIs
/// use, and the 409 sign-off conflict shape (`missing_or_stale_roles`)
/// gets the same wording ApprovalSurface always rendered for it.
export function describeWriteFailure(status: number, bodyText: string): string {
  const clip = (s: string): string =>
    s.length > MAX_BODY_CHARS ? `${s.slice(0, MAX_BODY_CHARS)}…` : s;
  try {
    const parsed: unknown = JSON.parse(bodyText);
    if (typeof parsed === 'string' && parsed.trim()) {
      return `HTTP ${status} — ${clip(parsed.trim())}`;
    }
    if (parsed && typeof parsed === 'object') {
      const rec = parsed as Record<string, unknown>;
      const roles = rec['missing_or_stale_roles'];
      if (Array.isArray(roles) && roles.length > 0) {
        return `sign-offs outstanding: ${roles.join(', ')}`;
      }
      for (const key of ['error', 'message', 'detail']) {
        const v = rec[key];
        if (typeof v === 'string' && v.trim()) {
          return `HTTP ${status} — ${clip(v.trim())}`;
        }
      }
    }
  } catch {
    // Not JSON — fall through to plain text.
  }
  const text = bodyText.trim();
  return text ? `HTTP ${status} — ${clip(text)}` : `HTTP ${status}`;
}

/// fetch that can only come back as a StepWriteResult: non-ok statuses
/// and thrown network errors both land in `failed` with a message fit
/// for inline rendering. It cannot be ignored by accident — the caller
/// has to branch to get anything out of it.
export async function writeStep(
  url: string,
  init: RequestInit,
): Promise<StepWriteResult> {
  try {
    const response = await fetch(url, init);
    if (!response.ok) {
      const text = await response.text().catch(() => '');
      return { kind: 'failed', error: describeWriteFailure(response.status, text) };
    }
    return { kind: 'ok', response };
  } catch (e) {
    const detail = e instanceof Error ? e.message : String(e);
    return { kind: 'failed', error: `network error — ${detail}` };
  }
}

/// The standard step PUT (PATCH semantics server-side).
export function putStep(
  jobId: string,
  stepId: string,
  body: unknown,
): Promise<StepWriteResult> {
  return writeStep(`/api/jobs/${jobId}/steps/${stepId}`, {
    method: 'PUT',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(body),
  });
}
