// Pure session logic, deliberately free of runes.
//
// classifyProbe and guestEmployee are ordinary functions: given a
// gateway probe body and a roster, they decide who you are. They used
// to live in session.svelte.ts beside `export const session =
// $state(...)`, and a module-level rune makes the whole module
// unloadable outside the Svelte compiler - so the seven test files in
// this package could not be run by `bun test` at all, and were wired
// into no CI job as a result. Splitting the pure half out is what
// makes them testable; session.svelte.ts re-exports everything here
// so its 26 importers are untouched.

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

export type SessionEnvelope = {
  value: SessionState;
  roster: ReadonlyArray<Employee>;
  fromGateway: boolean;
  /// True for the audit-readonly guest: every read surface renders,
  /// and surfaces that offer writes may hide or soften them.
  readonly: boolean;
};


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
