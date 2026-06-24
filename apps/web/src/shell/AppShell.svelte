<script lang="ts">
  // App shell — persistent sidebar + content slot.
  //
  // Sidebar layout: a Work section (operator-tier surfaces tied to
  // the user's role + assignments) + a flat list of Browse/Know
  // surfaces. The legacy Admin tier was removed 2026-05-03 — admin-
  // shaped pages live in the regular sidebar gated by the policy
  // role check.

  import { session } from '../session/session.svelte';
  import { moduleEnabled, getLabel } from '../session/manifest.svelte';
  import { canSeeRoute, type RouteName, type Role } from '../session/permissions';
  import { workForRole } from '../session/work-by-role';
  import { navigate } from '../router';
  import PersonaSwitcher from '../session/PersonaSwitcher.svelte';
  import SimClockBadge from './SimClockBadge.svelte';

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
    jobs:      { id: 'jobs',      label: 'All jobs',         path: '/jobs',      permKey: 'jobs' },
    sales:     { id: 'sales',     label: 'Sales pipeline',   path: '/sales',     permKey: 'sales' },
    service:   { id: 'service',   label: 'Service queue',    path: '/service',   permKey: 'service',   module: 'support' },
    refurb:    { id: 'refurb',    label: 'Refurbishment',    path: '/refurb',    permKey: 'refurb',    module: 'support' },
    qa:        { id: 'qa',        label: 'QA',               path: '/qa',        permKey: 'qa',        module: 'qa' },
    finance:   { id: 'finance',   label: 'Finance',          path: '/finance',   permKey: 'finance',   module: 'finance' },
    warehouse: { id: 'warehouse', label: 'Inventory',        path: '/warehouse', permKey: 'warehouse', module: 'warehouse' },
    shipping:  { id: 'shipping',  label: 'Shipments',        path: '/shipping',  permKey: 'shipping',  module: 'shipping' },
    support:   { id: 'support',   label: 'Support',          path: '/support',   permKey: 'support',   module: 'support' },
    ops:       { id: 'ops',       label: 'Operations',       path: '/ops',       permKey: 'ops' },
    exec:      { id: 'exec',      label: 'Exec',             path: '/exec',      permKey: 'exec',      module: 'exec' },
    'it-monitoring': { id: 'it-monitoring', label: 'IT Monitoring', path: '/it/monitoring', permKey: 'it-monitoring' },
    schedule:  { id: 'schedule',  label: 'My schedule',      path: '/calendar/me', permKey: 'schedule' },
    catalog:   { id: 'catalog',   label: 'Equipment',        path: '/catalog',   permKey: 'catalog',   module: 'equipment' },
    parts:     { id: 'parts',     label: 'Ingredients & parts', path: '/parts',  permKey: 'parts',     module: 'parts' },
    products:  { id: 'products',  label: 'Products',         path: '/products',  permKey: 'parts',     module: 'parts' },
    accounts:  { id: 'accounts',  label: 'Accounts',         path: '/accounts',  permKey: 'accounts' },
    vendors:   { id: 'vendors',   label: 'Vendors',          path: '/vendors',   permKey: 'vendors' },
    people:    { id: 'people',    label: 'Employees',        path: '/people',    permKey: 'people' },
    assets:    { id: 'assets',    label: 'Assets',             path: '/assets',    permKey: 'assets',    module: 'equipment' },
    shop:      { id: 'shop',      label: 'Shop',             path: '/shop',      permKey: 'shop' },
    inbox:     { id: 'inbox',     label: 'Inbox',            path: '/inbox',     permKey: 'inbox' },
    // 'it-sim' retired 2026-05-03 with boss-sim-api (HumanWorker step 9b).
    'marketing-assets': { id: 'marketing-assets', label: 'Marketing assets', path: '/marketing-assets', permKey: 'marketing-assets', module: 'marketing-assets' },
    calendar:  { id: 'calendar',  label: 'Release calendar', path: '/calendar',  permKey: 'calendar',  module: 'calendar' },
    // Modeling surfaces — operator-tier (no separate /admin tier).
    // policy + job-kinds are dept-head + COO authority (per the
    // "engineers are operators like anyone else" frame). Step
    // plugins are JS bundle authoring → IT engineering work.
    policy:               { id: 'policy',               label: 'Policy',              path: '/policy',         permKey: 'policy' },
    'job-kinds':          { id: 'job-kinds',            label: 'Job kinds',           path: '/job-kinds',      permKey: 'job-kinds' },
    'it-step-plugins':    { id: 'it-step-plugins',      label: 'Step plugins',        path: '/it/step-plugins', permKey: 'it-step-plugins' },
    'it-dispatcher':      { id: 'it-dispatcher',        label: 'Dispatcher rules',    path: '/it/dispatcher',  permKey: 'it-dispatcher' },
    'it-subjects':        { id: 'it-subjects',          label: 'Subjects & Classes',  path: '/it/subjects',    permKey: 'it-subjects' },
    // The rule-authoring list + editor are reached via a link FROM the
    // cascade viz (the it-dispatcher Surface entry), not their own sidebar
    // rows — so these catalog entries exist to satisfy the
    // Record<RouteName,…> type but are intentionally absent from
    // SURFACE_ORDER (no sidebar item ⇒ no sidebar-consistency entry).
    'it-dispatcher-rules': { id: 'it-dispatcher-rules', label: 'Dispatcher rules — authoring', path: '/it/dispatcher/rules', permKey: 'it-dispatcher-rules' },
    'it-dispatcher-rule':  { id: 'it-dispatcher-rule',  label: 'Dispatcher rule — editor',    path: '/it/dispatcher/rules', permKey: 'it-dispatcher-rule' },
    'it-design':          { id: 'it-design',            label: 'Design review',       path: '/it/design',      permKey: 'it-design' },
    'it-kb':              { id: 'it-kb',                label: 'IT Knowledge Base',   path: '/it/kb',          permKey: 'it-kb' },
    'auth-admin':         { id: 'auth-admin',           label: 'Auth admin',          path: '/auth-admin',     permKey: 'auth-admin' },
    // KB view of every active JobKind — read-only catalog,
    // visible to every role via canSeeRoute() short-circuit.
    // Editing lives at /job-kinds (Surface, gated to dept heads +
    // COO who author their own dept's work types).
    workflows:            { id: 'workflows',          label: 'Workflows',           path: '/workflows',     permKey: 'workflows' },
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
    'it-monitoring', // IT department live state — service map, perf, events, atlas
    'it-step-plugins', // IT — custom step UX bundles
    'it-dispatcher', // IT — dispatcher rule cascade (read-only)
    'it-subjects', // IT — SubjectKind taxonomy + Class registry (read-only)
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
      { id: 'manual', label: 'Company manual', path: '/manual', permKey: 'inbox' },
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
      ROUTE_CATALOG['it-kb'],
      // Design review — brings back the workflow that was retired
      // 2026-05-03. Lists every docs/design/*.md with parsed open
      // questions + the in-flight design-doc-review Job (if any).
      // The "system modeling its own development" claim depends on
      // this surface existing.
      ROUTE_CATALOG['it-design'],
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

  let { activeSection, children } = $props<{
    activeSection: string;
    children: () => any;
  }>();

  let user = $derived(
    session.value.kind === 'ready' ? session.value.user : null,
  );
  let role = $derived((user?.role ?? null) as Role | null);

  // Whether the visitor is logged in via BOSS local-auth (vs being
  // an anonymous demo-session visitor). Probes `/api/auth/me`,
  // which the gateway returns 401 for demo sessions (per the
  // 2026-05-25 fix). The result drives the top-bar Sign-in/out
  // button: showing "Sign out" to someone who never signed in is
  // confusing, and the demo-mode session would be immediately
  // re-minted on the next request anyway.
  let isLoggedIn = $state<boolean>(false);
  $effect(() => {
    (async () => {
      try {
        const r = await fetch('/api/auth/me');
        isLoggedIn = r.ok;
      } catch {
        isLoggedIn = false;
      }
    })();
  });

  // Work group is role-keyed: each role gets a tailored 3-5 item
  // list of the surfaces they personally operate from. The same
  // visible() filter still applies, so a brewery manifest that turns
  // off a module hides it from Work too.
  const WORK = $derived<NavGroup>({
    label: 'Work',
    items: workForRole(role).map((r) => ROUTE_CATALOG[r]),
  });

  let MAIN = $derived<ReadonlyArray<NavGroup>>([WORK, BROWSE, KNOW]);

  function visible(items: ReadonlyArray<NavItem>): ReadonlyArray<NavItem> {
    if (!role) return [];
    return items.filter((i) => {
      const policyOk = i.permKey === undefined || canSeeRoute(role, i.permKey);
      const moduleOk = i.module === undefined || moduleEnabled(i.module);
      return policyOk && moduleOk;
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
    <a
      class="shell-sidebar-header"
      href="/"
      onclick={(e) => onLinkClick(e, '/')}
      aria-label="Algedonic Ales — home"
    >
      <div class="shell-logo">Algedonic</div>
      <div class="shell-logo-sub">Ales</div>
    </a>

    <nav class="shell-nav">
      <div class="shell-nav-personal">
        <a
          href="/me"
          class="shell-nav-item shell-nav-home {activeSection === 'me' ? 'shell-nav-item-active' : ''}"
          onclick={(e) => onLinkClick(e, '/me')}
        >
          My Day
        </a>
        <a
          href="/inbox"
          class="shell-nav-item {activeSection === 'inbox' ? 'shell-nav-item-active' : ''}"
          onclick={(e) => onLinkClick(e, '/inbox')}
        >
          Inbox
        </a>
        <a
          href="/shop"
          class="shell-nav-item {activeSection === 'shop' ? 'shell-nav-item-active' : ''}"
          onclick={(e) => onLinkClick(e, '/shop')}
        >
          Shop
        </a>
      </div>

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
    <header class="shell-topbar">
      <div class="shell-topbar-left">
        <PersonaSwitcher />
      </div>
      <div class="shell-topbar-right">
        {#if isLoggedIn}
          <button class="shell-logout-btn" onclick={async () => {
            try { await fetch('/api/auth/logout', { method: 'POST' }); }
            catch {}
            window.location.href = '/login';
          }}>Sign out</button>
        {:else}
          <a class="shell-logout-btn" href={'/login'}>Sign in</a>
        {/if}
      </div>
    </header>
    <div class="shell-content">
      {@render children()}
    </div>
  </div>
  <SimClockBadge />
</div>

<style>
  .shell-logout-btn {
    background: transparent;
    border: 1px solid #d6d3d1;
    border-radius: 6px;
    padding: 5px 12px;
    font-size: 12px;
    color: #44403c;
    cursor: pointer;
  }
  .shell-logout-btn:hover {
    background: #f5f5f4;
    color: #1c1917;
  }
</style>
