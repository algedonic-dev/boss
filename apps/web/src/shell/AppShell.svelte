<script lang="ts">
  // App shell — persistent sidebar + content slot.
  //
  // Sidebar layout: a Work section (operator-tier surfaces tied to
  // the user's role + assignments) + a flat list of Browse/Know
  // surfaces. The legacy Admin tier was removed 2026-05-03 — admin-
  // shaped pages live in the regular sidebar gated by the policy
  // role check.

  import { session } from '@boss/web-kit/session/session.svelte';
  import { moduleEnabled, getLabel } from '@boss/web-kit/session/manifest.svelte';
  import { canSeeRoute, type RouteName, type Role } from '@boss/web-kit/session/permissions';
  import { workForRole } from '@boss/web-kit/session/work-by-role';
  import { navigate } from '../router';
  import PersonaSwitcher from '../session/PersonaSwitcher.svelte';

  type NavItem = Readonly<{
    id: string;
    label: string;
    path: string;
    permKey?: RouteName;
    /// Tenant module that this nav entry belongs to. When the
    /// manifest disables the module (e.g. brewery turns off
    /// `equipment` and `shipping`), the entry is hidden. Items
    /// without a module field are always-on (e.g. /jobs).
    module?: string;
  }>;
  type NavGroup = Readonly<{ label: string; items: ReadonlyArray<NavItem> }>;

  // ROUTE_CATALOG is the single registry of every routable nav entry —
  // its label, path, permKey, and tenant-module gate. Both Work
  // (role-keyed) and Surfaces (department-keyed) compose entries from
  // this catalog by RouteName, which keeps labels + module gates from
  // drifting between the two groups.
  const ROUTE_CATALOG: Readonly<Record<RouteName, NavItem>> = {
    jobs:      { id: 'jobs',      label: 'All jobs',         path: '/ux/jobs',      permKey: 'jobs' },
    sales:     { id: 'sales',     label: 'Sales pipeline',   path: '/ux/sales',     permKey: 'sales' },
    service:   { id: 'service',   label: 'Service queue',    path: '/ux/service',   permKey: 'service',   module: 'support' },
    refurb:    { id: 'refurb',    label: 'Refurbishment',    path: '/ux/refurb',    permKey: 'refurb',    module: 'support' },
    qa:        { id: 'qa',        label: 'QA',               path: '/ux/qa',        permKey: 'qa',        module: 'qa' },
    finance:   { id: 'finance',   label: 'Finance',          path: '/ux/finance',   permKey: 'finance',   module: 'finance' },
    warehouse: { id: 'warehouse', label: 'Inventory',        path: '/ux/warehouse', permKey: 'warehouse', module: 'warehouse' },
    shipping:  { id: 'shipping',  label: 'Shipments',        path: '/ux/shipping',  permKey: 'shipping',  module: 'shipping' },
    support:   { id: 'support',   label: 'Support',          path: '/ux/support',   permKey: 'support',   module: 'support' },
    ops:       { id: 'ops',       label: 'Operations',       path: '/ux/ops',       permKey: 'ops' },
    exec:      { id: 'exec',      label: 'Exec',             path: '/ux/exec',      permKey: 'exec',      module: 'exec' },
    'system-monitoring': { id: 'system-monitoring', label: 'Monitoring', path: '/system/monitoring', permKey: 'system-monitoring' },
    schedule:  { id: 'schedule',  label: 'My schedule',      path: '/ux/calendar/me', permKey: 'schedule' },
    catalog:   { id: 'catalog',   label: 'Equipment',        path: '/ux/catalog',   permKey: 'catalog',   module: 'equipment' },
    parts:     { id: 'parts',     label: 'Ingredients & parts', path: '/ux/parts',  permKey: 'parts',     module: 'parts' },
    products:  { id: 'products',  label: 'Products',         path: '/ux/products',  permKey: 'parts',     module: 'parts' },
    accounts:  { id: 'accounts',  label: 'Accounts',         path: '/ux/accounts',  permKey: 'accounts' },
    vendors:   { id: 'vendors',   label: 'Vendors',          path: '/ux/vendors',   permKey: 'vendors' },
    people:    { id: 'people',    label: 'Employees',        path: '/ux/people',    permKey: 'people' },
    assets:    { id: 'assets',    label: 'Assets',             path: '/ux/assets',    permKey: 'assets',    module: 'equipment' },
    shop:      { id: 'shop',      label: 'Shop',             path: '/ux/shop',      permKey: 'shop' },
    inbox:     { id: 'inbox',     label: 'Inbox',            path: '/ux/inbox',     permKey: 'inbox' },
    // 'it-sim' retired 2026-05-03 with boss-sim-api (HumanWorker step 9b).
    'marketing-assets': { id: 'marketing-assets', label: 'Marketing assets', path: '/ux/marketing-assets', permKey: 'marketing-assets', module: 'marketing-assets' },
    calendar:  { id: 'calendar',  label: 'Release calendar', path: '/ux/calendar',  permKey: 'calendar',  module: 'calendar' },
    // Modeling surfaces — operator-tier (no separate /admin tier).
    // policy + job-kinds are dept-head + COO authority (per the
    // "engineers are operators like anyone else" frame). Step
    // plugins are JS bundle authoring → IT engineering work.
    policy:               { id: 'policy',               label: 'Policy',              path: '/system/policy',  permKey: 'policy' },
    'job-kinds':          { id: 'job-kinds',            label: 'Job kinds',           path: '/system/job-kinds', permKey: 'job-kinds' },
    'system-step-plugins':    { id: 'system-step-plugins',      label: 'Step plugins',        path: '/system/step-plugins', permKey: 'system-step-plugins' },
    'system-dispatcher':      { id: 'system-dispatcher',        label: 'Dispatcher rules',    path: '/system/dispatcher',  permKey: 'system-dispatcher' },
    'system-model':          { id: 'system-model',            label: 'System Model',        path: '/system',             permKey: 'system-model' },
    'system-subjects':        { id: 'system-subjects',          label: 'Subjects & Classes',  path: '/system/subjects',    permKey: 'system-subjects' },
    // The rule-authoring list + editor are reached via a link FROM the
    // cascade viz (the system-dispatcher Surface entry), not their own sidebar
    // rows — so these catalog entries exist to satisfy the
    // Record<RouteName,…> type but are intentionally absent from
    // SURFACE_ORDER (no sidebar item ⇒ no sidebar-consistency entry).
    'system-dispatcher-rules': { id: 'system-dispatcher-rules', label: 'Dispatcher rules — authoring', path: '/system/dispatcher/rules', permKey: 'system-dispatcher-rules' },
    'system-dispatcher-rule':  { id: 'system-dispatcher-rule',  label: 'Dispatcher rule — editor',    path: '/system/dispatcher/rules', permKey: 'system-dispatcher-rule' },
    'system-design':          { id: 'system-design',            label: 'Design review',       path: '/system/design',      permKey: 'system-design' },
    // The "Evolve" surface — controlled, sandboxed model modifications
    // (placeholder for now; visible to every role via canSeeRoute).
    'system-experiments':     { id: 'system-experiments',       label: 'Experiments',         path: '/system/experiments', permKey: 'system-experiments' },
    'system-kb':              { id: 'system-kb',                label: 'Knowledge Base',      path: '/system/kb',          permKey: 'system-kb' },
    'auth-admin':         { id: 'auth-admin',           label: 'Auth admin',          path: '/system/auth-admin', permKey: 'auth-admin' },
    // KB view of every active JobKind — read-only catalog,
    // visible to every role via canSeeRoute() short-circuit.
    // Editing lives at /job-kinds (Surface, gated to dept heads +
    // COO who author their own dept's work types).
    workflows:            { id: 'workflows',          label: 'Workflows',           path: '/system/workflows', permKey: 'workflows' },
  };

  // Surfaces — one entry per department-rooted dashboard, in the
  // order an operator would scan them. Rendered as-is; the visible()
  // filter then drops anything the role/manifest blocks. A
  // service-only persona simply sees Service + Inventory + Shipments.
  const SURFACE_ORDER: ReadonlyArray<RouteName> = [
    'exec',       // executive
    'sales',      // sales department
    'service',    // service department
    'qa',         // quality
    'warehouse',  // warehouse + inventory
    'shipping',   // shipping department
    'support',    // support department
    'finance',    // finance department
    'system-model', // System Model hub (the landing, leads the cluster)
    'system-monitoring', // live state — service map, perf, events, atlas
    'system-step-plugins', // custom step UX bundles
    'system-dispatcher', // dispatcher rule cascade (read-only)
    'system-subjects', // SubjectKind taxonomy + Class registry (read-only)
    // 'it-sim' retired 2026-05-03 with boss-sim-api (HumanWorker step 9b).
    'ops',        // operations
    'policy',     // dept heads + COO — author role/scope policy
    'job-kinds',  // dept heads + COO — model the dept's work types
    'auth-admin', // dept heads + COO + IT — onboard / reset credentials
  ];

  const KNOW: NavGroup = {
    label: 'Knowledge Bases',
    items: [
      ROUTE_CATALOG.catalog,
      ROUTE_CATALOG.parts,
      ROUTE_CATALOG.products,
      ROUTE_CATALOG.accounts,
      ROUTE_CATALOG.vendors,
      ROUTE_CATALOG.people,
      ROUTE_CATALOG['marketing-assets'],
      ROUTE_CATALOG.calendar,
      { id: 'manual', label: 'Company manual', path: '/ux/manual', permKey: 'inbox' },
      // Workflows = KB of every active JobKind — everyone's
      // read-only catalog of "what kinds of work does this place
      // run?" Pairs with the /job-kinds Surface (editor), which
      // is gated to dept heads + COO + the C-suite catch-all.
      ROUTE_CATALOG.workflows,
      // IT Knowledge Base — department-rooted KB carrying ADRs,
      // architecture diagrams, hardware/software/provider
      // reference. Replaces the old /design + /architecture
      // entries: ADRs and the architecture diagrams now live
      // under the IT department surface (paired with /it/monitoring
      // for live state).
      ROUTE_CATALOG['system-kb'],
      // Design review — brings back the workflow that was retired
      // 2026-05-03. Lists every docs/design/*.md with parsed open
      // questions + the in-flight design-doc-review Job (if any).
      // The "system modeling its own development" claim depends on
      // this surface existing.
      ROUTE_CATALOG['system-design'],
    ],
  };

  const BROWSE: NavGroup = {
    label: 'Surfaces',
    items: SURFACE_ORDER.map((r) => ROUTE_CATALOG[r]),
  };

  // /admin tier removed entirely (2026-05-03). Engineers and
  // platform operators are operators like anyone else; their
  // surfaces sit alongside the rest in the same Surfaces group:
  //   - /policy + /job-kinds → modeling surfaces, gated to
  //     dept heads + COO + C-suite (NOT IT — those decisions
  //     are operational, not technical).
  //   - /it/monitoring + /it/kb + /it/step-plugins + /it/sim →
  //     the IT department's surface set. Engineers run the
  //     platform; their "Work" looks like everyone else's.
  // Future surfaces should NOT bring back the Admin tier; pick
  // a department-rooted slug and a role-gated permKey.

  let { activeSection, perspective = 'user', children } = $props<{
    activeSection: string;
    // Which top-level perspective tab this shell renders under. Drives
    // which surfaces appear in the sidebar.
    perspective?: 'model' | 'user';
    children: () => any;
  }>();

  let user = $derived(
    session.value.kind === 'ready' ? session.value.user : null,
  );
  let role = $derived((user?.role ?? null) as Role | null);

  // Work group is role-keyed: each role gets a tailored 3-5 item
  // list of the surfaces they personally operate from. The same
  // visible() filter still applies, so a brewery manifest that turns
  // off a module hides it from Work too.
  const WORK = $derived<NavGroup>({
    label: 'Work',
    items: workForRole(role).map((r) => ROUTE_CATALOG[r]),
  });

  // System Model perspective — surfaces grouped by the aspects of
  // operating the model: Run (observe the live machine), Define
  // (configure the model), Evolve (controlled change + experiments),
  // Platform (reference + admin). The User Experiences perspective
  // keeps Work / Surfaces / Knowledge Bases (below). Selected via the
  // `perspective` prop.
  const MODEL_GROUPS: ReadonlyArray<NavGroup> = [
    {
      label: 'Run',
      items: [ROUTE_CATALOG['system-model'], ROUTE_CATALOG['system-monitoring']],
    },
    {
      label: 'Define',
      items: [
        ROUTE_CATALOG.workflows,
        ROUTE_CATALOG['job-kinds'],
        ROUTE_CATALOG['system-subjects'],
        ROUTE_CATALOG['system-step-plugins'],
        ROUTE_CATALOG['system-dispatcher'],
        ROUTE_CATALOG.policy,
      ],
    },
    {
      label: 'Evolve',
      items: [ROUTE_CATALOG['system-experiments'], ROUTE_CATALOG['system-design']],
    },
    {
      label: 'Platform',
      items: [ROUTE_CATALOG['system-kb'], ROUTE_CATALOG['auth-admin']],
    },
  ];

  let MAIN = $derived<ReadonlyArray<NavGroup>>(
    perspective === 'model' ? MODEL_GROUPS : [WORK, BROWSE, KNOW],
  );

  // Perspective split: which surfaces belong to the System Model tab
  // (the model's configuration + how it's running — most of what used
  // to be "IT") vs the User Experiences tab (the actor work surfaces +
  // knowledge bases — Finance, Inventory, the KBs, …). Keyed by
  // permKey/RouteName. Keep in sync with App.svelte's MODEL_KINDS,
  // which classifies the same split by route kind to drive the active
  // tab — the two must agree for every routed surface.
  const MODEL_ROUTES = new Set<RouteName>([
    'system-model', 'system-monitoring', 'system-step-plugins', 'system-dispatcher',
    'system-subjects', 'system-dispatcher-rules', 'system-dispatcher-rule',
    'system-kb', 'system-design', 'system-experiments', 'policy', 'job-kinds', 'workflows', 'auth-admin',
  ]);
  function inPerspective(i: NavItem): boolean {
    const isModel = i.permKey !== undefined && MODEL_ROUTES.has(i.permKey);
    return perspective === 'model' ? isModel : !isModel;
  }

  function visible(items: ReadonlyArray<NavItem>): ReadonlyArray<NavItem> {
    if (!role) return [];
    return items.filter((i) => {
      const policyOk = i.permKey === undefined || canSeeRoute(role, i.permKey);
      const moduleOk = i.module === undefined || moduleEnabled(i.module);
      return policyOk && moduleOk && inPerspective(i);
    });
  }

  function onLinkClick(e: MouseEvent, path: string) {
    if (e.metaKey || e.ctrlKey || e.shiftKey || e.button !== 0) return;
    e.preventDefault();
    navigate(path);
  }
</script>

<div class="app-shell">
  <aside class="shell-sidebar">
    <nav class="shell-nav">
      {#if perspective === 'user'}
        <div class="shell-nav-personal">
          <a
            href="/ux/me"
            class="shell-nav-item shell-nav-home {activeSection === 'me' ? 'shell-nav-item-active' : ''}"
            onclick={(e) => onLinkClick(e, '/ux/me')}
          >
            My Day
          </a>
          <a
            href="/ux/inbox"
            class="shell-nav-item {activeSection === 'inbox' ? 'shell-nav-item-active' : ''}"
            onclick={(e) => onLinkClick(e, '/ux/inbox')}
          >
            Inbox
          </a>
          <a
            href="/ux/shop"
            class="shell-nav-item {activeSection === 'shop' ? 'shell-nav-item-active' : ''}"
            onclick={(e) => onLinkClick(e, '/ux/shop')}
          >
            Shop
          </a>
        </div>
      {/if}

      {#each MAIN as group (group.label)}
        {@const items = visible(group.items)}
        {#if items.length > 0}
          <div class="shell-nav-group">
            <div class="shell-nav-group-label">
              <span class="shell-nav-group-chevron">▾</span>
              {group.label}
            </div>
            {#each items as item (item.id)}
              <a
                href={item.path}
                class="shell-nav-item {activeSection === item.id ? 'shell-nav-item-active' : ''}"
                onclick={(e) => onLinkClick(e, item.path)}
              >
                {getLabel(`nav.${item.id}_label`, item.label)}
              </a>
            {/each}
          </div>
        {/if}
      {/each}
    </nav>


    <div class="shell-sidebar-footer">
      {#if user}
        <div class="shell-user">
          <div class="shell-user-name">{user.name}</div>
          <div class="shell-user-role">{user.role}</div>
        </div>
      {/if}
    </div>
  </aside>

  <div class="shell-main">
    <!-- Demo-mode persona switcher — fixed-positioned (bottom-left),
         so it renders here but floats independently of the layout.
         The system-time + sign-in chrome moved up to the perspective
         tab bar; the old topbar is gone. -->
    <PersonaSwitcher />
    <div class="shell-content">
      {@render children()}
    </div>
  </div>
</div>
