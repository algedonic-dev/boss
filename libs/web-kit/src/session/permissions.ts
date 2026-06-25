// Role-based permissions — verbatim port of
// apps/web/src/session/permissions.ts, minus the MyScope wiring
// which is stubbed for phase 1.
//
// Two layers today:
//   1. ROUTE_ACCESS — static matrix of which top-level sections a
//      role can even see.
//   2. can(action, resource) — finer-grained checks inside a view.
// Phase 2 brings back the policy-scope overlay from the React app.

// A role code. Roles are tenant-extensible reference data owned by the
// Class registry (subject_kind=employee, member_attribute=role); the
// SPA reads the live list + labels via `classesFor('employee', 'role')`
// rather than re-declaring them here, so adding a tenant role no longer
// needs a TS bundle rebuild. The maps below (ROUTE_ACCESS, WORK_BY_ROLE)
// are SPA route-visibility *policy*, not the role vocabulary — they stay
// hand-curated and key on this open string. A role with no entry falls
// through to the defensive defaults in `canSeeRoute` / `workForRole`.
export type Role = string;

export type RouteName =
  | 'shop' | 'exec' | 'catalog' | 'accounts' | 'assets' | 'sales' | 'service'
  | 'refurb' | 'parts' | 'products' | 'finance' | 'people' | 'qa' | 'warehouse' | 'support' | 'ops'
  | 'system-monitoring' | 'inbox' | 'shipping'
  | 'vendors' | 'marketing-assets' | 'calendar' | 'schedule' | 'jobs'
  // Platform-administration surfaces. Same `permKey: 'it'` gate
  // as the legacy ADMIN footer; these route names exist so the
  // surfaces can land in role-keyed Work lists per the
  // three-axis IA simplifier ("administering is someone's job").
  | 'policy' | 'job-kinds' | 'system-step-plugins' | 'system-dispatcher' | 'system-design'
  // The model-vocabulary surface — SubjectKind taxonomy + Class registry
  // (read-only). Same `it-*` audience as the dispatcher cascade it sits beside.
  | 'system-subjects'
  // The IT landing — a live-stats hub linking the model-reading surfaces.
  | 'system-model'
  // Dispatcher rule-authoring surfaces — same `it-dispatcher` audience as
  // the cascade viz they hang off (reached via links from it, not their
  // own sidebar entries).
  | 'system-dispatcher-rules' | 'system-dispatcher-rule'
  | 'system-kb' | 'auth-admin'
  | 'workflows';

const ALL: ReadonlyArray<RouteName> = [
  'shop', 'exec', 'catalog', 'accounts', 'assets', 'sales', 'service', 'refurb',
  'parts', 'products', 'finance', 'people', 'qa', 'warehouse', 'support', 'ops', 'system-monitoring',
  'shipping', 'vendors', 'marketing-assets', 'calendar',
  'schedule', 'jobs',
  'policy', 'job-kinds', 'system-step-plugins', 'system-dispatcher',
  'system-dispatcher-rules', 'system-dispatcher-rule', 'system-design', 'system-subjects', 'system-model', 'system-kb', 'auth-admin',
  'workflows',
];

export const ROUTE_ACCESS: Record<Role, ReadonlyArray<RouteName>> = {
  ceo: ALL,
  cto: ALL,
  coo: ALL,
  // Platform super-admin. The policy layer grants `platform-admin`
  // Scope::All on every resource (boss-policy-client defaults), so the
  // sidebar should surface every section — same rationale as
  // `audit-readonly` below; server-side enforces the real grants. This
  // is also the role the `job-kind-design` approve step requires, so the
  // operator authoring a JobKind lands here and needs the full surface.
  'platform-admin': ALL,
  cfo: ['exec', 'accounts', 'assets', 'sales', 'finance', 'people', 'parts', 'products', 'vendors'],
  'vp-sales': ['exec', 'catalog', 'accounts', 'assets', 'sales', 'people'],
  'sales-mgr': ['catalog', 'accounts', 'assets', 'sales', 'people'],
  'sales-rep': ['catalog', 'accounts', 'sales'],
  'service-mgr': ['exec', 'catalog', 'accounts', 'assets', 'service', 'refurb', 'parts', 'products', 'vendors', 'people', 'support', 'shipping', 'schedule'],
  'service-tech': ['catalog', 'accounts', 'assets', 'service', 'parts', 'products', 'schedule'],
  'refurb-supervisor': ['catalog', 'assets', 'refurb', 'parts', 'products', 'people', 'qa'],
  'refurb-tech': ['catalog', 'assets', 'refurb', 'parts', 'products'],
  'qa-lead': ['catalog', 'assets', 'refurb', 'parts', 'products', 'people', 'qa'],
  'qa-tech': ['catalog', 'assets', 'refurb', 'parts', 'products', 'qa'],
  'warehouse-mgr': ['parts', 'products', 'refurb', 'people', 'warehouse', 'vendors', 'shipping'],
  'warehouse-clerk': ['parts', 'products', 'refurb', 'warehouse', 'shipping'],
  'parts-buyer': ['parts', 'products', 'refurb', 'warehouse', 'vendors', 'shipping'],
  controller: ['exec', 'accounts', 'sales', 'finance', 'parts', 'products', 'vendors'],
  'ap-specialist': ['parts', 'products', 'accounts', 'vendors', 'finance'],
  'hr-generalist': ['people'],
  recruiter: ['people'],
  'support-specialist': ['accounts', 'service', 'support'],
  // IT roles maintain the platform itself: monitoring, KB, step
  // plugins (JS bundles for custom step UX), simulator runs to
  // validate workflow changes before deploy. They do NOT get
  // policy or job-kinds — those belong to dept heads + COO who
  // model what their dept's work looks like (per the
  // "modeling-not-building" frame).
  'it-manager': ['exec', 'system-monitoring', 'system-kb',
    'system-step-plugins', 'system-dispatcher', 'system-dispatcher-rules', 'system-dispatcher-rule', 'system-subjects', 'system-model',
    'system-design', 'jobs'],
  auditor: ['finance', 'accounts', 'assets'],
  // Audit-readonly is the system audit account — Read on every
  // resource via the policy gate, so the sidebar surfaces every
  // section. The actual gating is enforced server-side; this
  // table just controls what's visible.
  'audit-readonly': ALL,
  owner: ALL,
  'smoke-tester': ALL,

  // ----- Brewery roles -----
  // Each brewery role gets the surfaces it actually needs to do
  // its job. The corresponding WORK_BY_ROLE entry then picks the
  // 2-4 it spends most time in for the personal "Work" group.

  // Production
  'head-brewer':   ['exec', 'jobs', 'parts', 'products', 'qa', 'people', 'schedule'],
  'senior-brewer': ['jobs', 'parts', 'products', 'qa', 'schedule'],
  brewer:          ['jobs', 'parts', 'products', 'schedule'],
  'cellar-tech':   ['jobs', 'parts', 'products', 'schedule'],
  'shift-lead':    ['jobs', 'parts', 'products', 'schedule', 'people'],

  // Packaging
  'packaging-mgr':  ['jobs', 'warehouse', 'shipping', 'people', 'schedule'],
  'packaging-tech': ['jobs', 'warehouse', 'schedule'],
  palletizer:       ['jobs', 'warehouse'],

  // QA / lab
  'qa-supervisor': ['jobs', 'qa', 'parts', 'products', 'people'],
  'lab-tech':      ['jobs', 'qa', 'parts', 'products'],

  // Warehouse
  'forklift-operator': ['warehouse', 'parts', 'products', 'shipping'],
  'inventory-clerk':   ['warehouse', 'parts', 'products'],
  'shipping-clerk':    ['warehouse', 'shipping'],

  // Distribution
  'distribution-driver': ['shipping', 'jobs', 'schedule'],

  // Maintenance
  'maintenance-mgr': ['jobs', 'parts', 'products', 'people', 'schedule'],
  electrician:       ['jobs', 'parts', 'products', 'schedule'],
  mechanic:          ['jobs', 'parts', 'products', 'schedule'],

  // Sales
  'account-manager': ['sales', 'accounts', 'jobs'],

  // Marketing
  'brand-manager':        ['exec', 'marketing-assets', 'calendar', 'jobs'],
  'events-coord':         ['calendar', 'jobs', 'marketing-assets'],
  'social-media-coord':   ['marketing-assets', 'calendar'],
  'marketing-mgr':        ['exec', 'marketing-assets', 'calendar', 'jobs'],
  'marketing-specialist': ['marketing-assets', 'calendar', 'jobs'],
  'content-writer':       ['marketing-assets'],
  'brand-designer':       ['marketing-assets'],

  // Taproom
  bartender:        ['calendar', 'schedule'],
  'taproom-server': ['calendar', 'schedule'],

  // Finance
  bookkeeper:    ['finance', 'accounts', 'vendors', 'jobs'],
  'ar-clerk':    ['finance', 'accounts', 'jobs'],
  'ap-clerk':    ['finance', 'vendors', 'parts', 'products', 'jobs'],
  'fp-analyst':  ['finance', 'exec', 'jobs'],
  'payroll-mgr': ['finance', 'people', 'jobs'],

  // People (HR)
  'benefits-coord': ['people', 'jobs'],

  // IT
  // IT-team roles own the platform's runtime: monitoring,
  // knowledge base, step plugins, simulator. They don't author
  // policy or JobKinds — that authority belongs to dept heads
  // + COO (the people whose work the JobKind models) per the
  // "engineers are operators like anyone else" framing.
  'it-director': ['exec', 'system-monitoring', 'system-kb',
    'system-step-plugins', 'system-dispatcher', 'system-dispatcher-rules', 'system-dispatcher-rule', 'system-subjects', 'system-model',
    'system-design', 'jobs'],
  sysadmin:      ['system-monitoring', 'system-kb',
    'system-step-plugins', 'system-dispatcher', 'system-dispatcher-rules', 'system-dispatcher-rule', 'system-subjects', 'system-model',
    'system-design', 'jobs'],
  helpdesk:      ['system-monitoring', 'system-kb', 'jobs'],

  // Heads of department.
  //
  // Dept heads + the COO are the only roles outside the C-suite
  // catch-all (CEO/CTO/COO = ALL) that get `policy` and
  // `job-kinds`. Rationale: these surfaces author *the company's
  // model of its own work* — what work types exist, what
  // role-scoped permissions apply. That authority belongs to
  // operational leaders, not the IT team that maintains the
  // platform. Server-side scope checks gate edits to the dept
  // head's own department; the SPA grant just controls what's
  // visible.
  'head-of-distribution': ['exec', 'shipping', 'jobs', 'people', 'policy', 'job-kinds'],
  'head-of-marketing':    ['exec', 'marketing-assets', 'calendar', 'jobs', 'people', 'policy', 'job-kinds'],
  'head-of-people':       ['exec', 'people', 'jobs', 'policy', 'job-kinds'],
  'head-of-sales':        ['exec', 'sales', 'accounts', 'jobs', 'people', 'policy', 'job-kinds'],
};

export function canSeeRoute(role: Role, route: RouteName): boolean {
  if (route === 'shop' || route === 'inbox' || route === 'workflows') return true;
  // Defensive default: a role we don't know about (a freshly-added
  // class registry entry the SPA hasn't been re-bundled for) sees
  // nothing rather than crashing on `.includes` of undefined.
  const access = ROUTE_ACCESS[role];
  return access ? access.includes(route) : false;
}
