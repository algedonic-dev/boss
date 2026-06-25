// Current-user session.
//
// Resolution order:
//   1. Gateway `/api/session` → resolved `employee_id` → Employee row
//   2. Demo-mode localStorage persona override
//   3. Fallback to the first active employee (first-boot safety net)
//
// The `setPersona` path writes to localStorage so a refresh keeps
// the chosen persona; selecting one flips off the "from gateway"
// bit so the header shows "viewing as … (demo)" cleanly.

const STORAGE_KEY = 'boss.persona.empId';
const DEFAULT_EMP_ID = 'emp-001'; // CEO

/// Name of the cookie that tells the dev-server / gateway which
/// persona the user is currently viewing as (demo mode only). The
/// dev-server looks this up in the roster and synthesises
/// x-boss-user from the matched employee's id + role + department
/// so backend policy scoping reflects the selected persona.
///
/// In a real (non-demo) deployment this cookie is ignored —
/// personas are a demo affordance.
const PERSONA_COOKIE = 'boss-persona';

function writePersonaCookie(id: string): void {
  // 30-day cookie scoped to the whole app. `SameSite=Lax` is
  // enough for same-origin fetches; no Secure because the dev
  // server is http.
  document.cookie = `${PERSONA_COOKIE}=${encodeURIComponent(id)}; path=/; max-age=2592000; SameSite=Lax`;
}

// BOSS_DEMO_MODE is baked in by the build step. Default true so that
// bun-run-dev works without a gateway; prod sets it to '0'.
export const DEMO_MODE: boolean = (() => {
  try {
    return (process as unknown as { env?: Record<string, string> }).env?.[
      'BOSS_DEMO_MODE'
    ] !== '0';
  } catch {
    return true;
  }
})();

export type Certification = {
  name: string;
  issuing_body: string;
  issued_on: string;
  expires_on: string | null;
};

export type Employee = {
  id: string;
  name: string;
  email: string;
  role: string;
  department: string;
  hire_date: string;
  status: string;
  location: string;
  employment_type: string;
  skill_level?: number | null;
  skills: string[];
  certifications: Certification[];
  manager_id?: string | null;
};

export type SessionState =
  | { kind: 'loading' }
  | { kind: 'ready'; user: Employee }
  | { kind: 'unauthenticated' }
  | { kind: 'unrecognized'; username: string };

type SessionEnvelope = {
  value: SessionState;
  roster: ReadonlyArray<Employee>;
  fromGateway: boolean;
};

export const session = $state<SessionEnvelope>({
  value: { kind: 'loading' },
  roster: [],
  fromGateway: false,
});

function readStoredPersona(byId: Map<string, Employee>): string {
  try {
    const s = localStorage.getItem(STORAGE_KEY);
    if (s && byId.has(s)) return s;
  } catch {
    // localStorage unavailable — fall through
  }
  return DEFAULT_EMP_ID;
}

export async function loadSession(): Promise<void> {
  // 1. Fetch the roster first — it's the universe for every lookup.
  let roster: Employee[] = [];
  try {
    const r = await fetch('/api/people');
    if (r.ok) roster = (await r.json()) as Employee[];
  } catch {
    // Empty roster still lets the gateway fall through.
  }
  const byId = new Map(roster.map((e) => [e.id, e]));
  session.roster = roster;

  // 2. Gateway session probe — a successful hit with a resolved
  //    employee_id wins.
  const storedPersona = readStoredPersona(byId);
  try {
    const r = await fetch('/api/session', { credentials: 'same-origin' });
    if (r.ok) {
      const body = (await r.json()) as {
        username?: string;
        employee_id?: string;
      };
      const username = body.username ?? '';
      // Use the gateway-resolved employee_id directly (set at
      // session-mint time from the auth provider's email lookup).
      const emp = body.employee_id ? byId.get(body.employee_id) ?? null : null;
      if (emp) {
        session.fromGateway = true;
        session.value = { kind: 'ready', user: emp };
        return;
      }
      if (DEMO_MODE && byId.has(storedPersona)) {
        session.fromGateway = false;
        session.value = { kind: 'ready', user: byId.get(storedPersona)! };
        return;
      }
      // In demo mode with a roster but no resolved employee_id +
      // no valid stored persona (fresh incognito visitor, or
      // stored persona pointing at a wiped employee), fall
      // through to the demo-mode preferred-persona path below
      // rather than landing in 'unrecognized'. The gateway's
      // default unauth session carries `username='demo@anonymous'`
      // which used to land here as unrecognized — that hid the
      // persona switcher from anyone who hadn't manually picked
      // one.
      if (username && !(DEMO_MODE && roster.length > 0)) {
        session.value = { kind: 'unrecognized', username };
        return;
      }
    }
  } catch {
    // Network failure → fall through to demo-mode path
  }

  // 3. Demo-mode fallback.
  if (DEMO_MODE && byId.has(storedPersona)) {
    session.fromGateway = false;
    session.value = { kind: 'ready', user: byId.get(storedPersona)! };
    // Mirror the chosen persona into the cookie so the gateway's
    // role_headers middleware sees the same id the SPA renders
    // as. Without this, `session.value.user.id` is the brewery
    // employee but `x-boss-user.id` stays "demo@anonymous", and
    // every "you can only act as yourself" check (messages
    // send, inbox read) returns 403.
    writePersonaCookie(storedPersona);
  } else if (DEMO_MODE && roster.length > 0) {
    session.fromGateway = false;
    // Prefer the CEO for the first-impression demo experience —
    // the playground's "what does Boss look like" page should
    // open as the operator, not as an audit-readonly system
    // account (which sorts alphabetically first as `emp-audit`
    // and otherwise wins via roster[0]).
    const preferred =
      roster.find((e) => e.role === 'ceo') ??
      roster.find((e) => e.role === 'cto') ??
      roster.find((e) => e.role === 'coo') ??
      roster.find((e) => !['audit-readonly', 'system'].includes(e.role)) ??
      roster[0]!;
    session.value = { kind: 'ready', user: preferred };
    // Same persona-cookie mirroring as the storedPersona branch
    // above — pin the backend's view of "who is this request" to
    // the brewery employee the SPA is rendering as.
    writePersonaCookie(preferred.id);
  } else {
    session.value = { kind: 'unauthenticated' };
  }
}

export function setPersona(id: string): void {
  try {
    localStorage.setItem(STORAGE_KEY, id);
  } catch {
    // localStorage unavailable — persona change still works for
    // the current tab, just doesn't persist.
  }
  // Also write a cookie so the dev-server + gateway can synthesise
  // the right x-boss-user header on API requests. Without this the
  // backend still saw the default (emp-001 CEO) and returned
  // unscoped data.
  try {
    writePersonaCookie(id);
  } catch {
    // document.cookie unavailable (SSR / non-browser) — safe to
    // skip; the UI still updates correctly.
  }
  const emp = session.roster.find((e) => e.id === id);
  if (emp) {
    session.fromGateway = false;
    session.value = { kind: 'ready', user: emp };
  }
}
