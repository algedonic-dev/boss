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
  import {
    ROUTE_CATALOG,
    type AppId,
    type NavItem,
    type NavGroup,
  } from './nav-catalog';

  // NavItem / NavGroup / ROUTE_CATALOG live in ./nav-catalog so both
  // this shell and App.svelte read the same registry — and so the
  // consistency test can import it instead of mirroring it by hand.
  // `app` on each entry is the single answer to "which tab owns this
  // surface"; it replaced this file's MODEL_ROUTES and App.svelte's
  // MODEL_KINDS, which had to agree and could silently stop agreeing.

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
    // Which app tab this shell renders under. Drives which surfaces
    // appear in the sidebar. Typed as the full AppId — the shell
    // speaks the same vocabulary as the catalog, so adding an app is
    // a catalog change rather than a widening here.
    perspective?: AppId;
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
      items: [
        ROUTE_CATALOG['system-model'],
        ROUTE_CATALOG['system-monitoring'],
        // Audit Log + Atlas are sub-pages of monitoring with no
        // distinct permKey — plain NavItems (permKey-less ⇒ always
        // visible + always in-perspective; see visible()/inPerspective()).
        { id: 'system-audit', label: 'Audit Log', path: '/system/monitoring/events' },
        { id: 'system-atlas', label: 'Atlas', path: '/system/monitoring/atlas' },
      ],
    },
    {
      label: 'Define',
      items: [
        // Workflows is the single UI surface for JobKinds: the
        // read-only catalog that also links into the authoring
        // routes (/system/job-kinds*). The separate "Job kinds"
        // sidebar entry was dropped — authoring is reached FROM
        // Workflows, not its own sidebar row.
        ROUTE_CATALOG.workflows,
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

  // A surface is in-perspective when its catalog `app` matches the
  // app this shell is rendering. One comparison against one field —
  // where this used to be a MODEL_ROUTES set here that had to agree
  // with a MODEL_KINDS set in App.svelte, keyed off a different
  // vocabulary (RouteName vs Route['kind']).
  function inPerspective(i: NavItem): boolean {
    // A permKey-less NavItem (e.g. a plain sub-page link like Audit
    // Log / Atlas) carries no app of its own — it belongs to whatever
    // group it's placed in, so it's always in-perspective.
    if (i.permKey === undefined) return true;
    return (ROUTE_CATALOG[i.permKey]?.app ?? 'user') === perspective;
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
