// Current-user session.
//
// One source: the gateway's `/api/session`, resolved to an Employee
// row. No session means unauthenticated — there is no fallback that
// invents an identity, because the fallback that used to be here is
// what let the chrome disagree with the server about who you were.
//
// `setPersona` survives for `bun run dev` and the smoke suite, where
// there is no gateway to issue a session. The dev-server reads the
// cookie it writes and synthesises `x-boss-user` from it. The gateway
// ignores it entirely.

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
  /// True for the audit-readonly guest: every read surface renders,
  /// and surfaces that offer writes may hide or soften them.
  readonly: boolean;
};

export const session = $state<SessionEnvelope>({
  value: { kind: 'loading' },
  roster: [],
  fromGateway: false,
  readonly: false,
});

/// The honest synthetic identity for a read-only visitor. The old
/// demo-mode sin was dressing a visitor in a REAL employee's name,
/// role and department; the fix is not to strip the visitor of a
/// renderable identity, it is to give them their own: named Guest,
/// carrying the audit-readonly role they actually hold, colliding
/// with no roster id, assignable to nothing.
export function guestEmployee(username: string): Employee {
  return {
    id: username,
    name: 'Guest',
    email: username,
    role: 'audit-readonly',
    department: 'visitor',
    hire_date: new Date().toISOString().slice(0, 10),
    status: 'active',
    location: '—',
    employment_type: 'guest',
    skills: [],
    certifications: [],
  };
}

export type ProbeBody = {
  username?: string;
  employee_id?: string;
  role?: string;
};

/// Pure classification of the gateway probe — extracted so the
/// guest/unrecognized boundary is a tested decision, not a branch
/// buried in a fetch handler.
export function classifyProbe(
  body: ProbeBody,
  byId: Map<string, Employee>,
): { value: SessionState; readonly: boolean } | null {
  const username = body.username ?? '';
  const emp = body.employee_id ? (byId.get(body.employee_id) ?? null) : null;
  if (emp) return { value: { kind: 'ready', user: emp }, readonly: false };
  // A session with no employee and the audit-readonly role is the
  // guest — a first-class read-only persona, not a broken login.
  if (username && body.role === 'audit-readonly') {
    return {
      value: { kind: 'ready', user: guestEmployee(username) },
      readonly: true,
    };
  }
  if (username) return { value: { kind: 'unrecognized', username }, readonly: false };
  return null;
}

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
      const body = (await r.json()) as ProbeBody;
      const classified = classifyProbe(body, byId);
      if (classified) {
        session.fromGateway = true;
        session.readonly = classified.readonly;
        session.value = classified.value;
        return;
      }
    }
  } catch {
    // Network failure → fall through to demo-mode path
  }

  // No session, no user. There used to be a demo-mode fallback here
  // that rendered a persona from localStorage — or, failing that, the
  // CEO — for anyone the gateway could not resolve. It is why a
  // read-only visitor saw an executive's name in the chrome while
  // every write returned 403, and it is the last piece of the mode
  // that made "who am I" a different question from "who does the
  // server think I am".
  session.value = { kind: 'unauthenticated' };
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
    // A persona switch is a full identity change. `readonly` belongs
    // to the guest identity, not to the tab — leaving it set kept a
    // once-guest session rendering GuestHome and inert write surfaces
    // after it became a real operator.
    session.readonly = false;
    session.value = { kind: 'ready', user: emp };
  }
}
