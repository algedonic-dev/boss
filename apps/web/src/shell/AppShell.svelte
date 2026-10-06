<script lang="ts">
  // App shell — persistent sidebar + content slot.
  //
  // Sidebar layout: a Work section (operator-tier surfaces tied to
  // the user's role + assignments) + a flat list of Browse/Know
  // surfaces. The legacy Admin tier was removed 2026-05-03 — admin-
  // shaped pages live in the regular sidebar gated by the policy
  // role check.

  import ClassesReadFailed from '@boss/web-kit/ui/ClassesReadFailed.svelte';
  import { session } from '@boss/web-kit/session/session.svelte';
  import { moduleEnabled, getLabel } from '@boss/web-kit/session/manifest.svelte';
  import { canSeeRoute, type Role } from '@boss/web-kit/session/permissions';
  import { departmentLabel } from '@boss/web-kit/nav';
  import { departments } from '@boss/web-kit/session/departments.svelte';
  import { href, navigate } from '../router';
  import {
    ROUTE_CATALOG,
    departmentRows,
    inPerspective,
    type AppId,
    type NavItem,
    type NavGroup,
  } from './nav-catalog';
  import { classesFor } from '@boss/web-kit/session/classes.svelte';
  import { safeLinkHref } from '@boss/web-kit/links';

  // NavItem / NavGroup / ROUTE_CATALOG live in ./nav-catalog so both
  // this shell and App.svelte read the same registry — and so the
  // consistency test can import it instead of mirroring it by hand.
  // `owner` on each entry is the single answer to "which tab owns this
  // surface"; it replaced this file's MODEL_ROUTES and App.svelte's
  // MODEL_KINDS, which had to agree and could silently stop agreeing.

  // Surfaces — one entry per department-rooted dashboard, in the
  // order an operator would scan them. Rendered as-is; the visible()
  // filter then drops anything the role/manifest blocks. A
  // service-only persona simply sees Service + Inventory + Shipments.
  let { activeSection, perspective = 'home', children } = $props<{
    activeSection: string;
    // Which app tab this shell renders under. Drives which surfaces
    // appear in the sidebar. Typed as the full AppId — the shell
    // speaks the same vocabulary as the catalog, so adding an app is
    // a catalog change rather than a widening here.
    perspective?: AppId;
    children: () => any;
  }>();

  // Svelte's generated props type widens `perspective` to `any` through
  // the default, which silently un-types every Record lookup below.
  // Pin it once.
  let activeApp = $derived(perspective as AppId);

  let user = $derived(
    session.value.kind === 'ready' ? session.value.user : null,
  );
  let role = $derived((user?.role ?? null) as Role | null);
  // The role's Class row: where a tenant narrows this role's sidebar
  // (`metadata.surfaces`, 18d6a6c9). Loaded at boot with the other
  // employee classes; undefined until then, which canSeeRoute reads as
  // "nothing declared" — every module-on surface, never an empty bar.
  let roleRow = $derived(classesFor('employee', 'role').find((r) => r.code === role));

  // Unread badge on Inbox (David, feedback 8c020e6d: "I can't see new
  // inbox messages").
  //
  // Counts `kind=direct` only, which is the same set the inbox itself
  // opens on. The unfiltered count for the platform admin is ~1,980
  // against 3 directs, so a badge wired to every kind would render the
  // noise as a number and be ignored within a day — the exact failure
  // the needs-you filter already exists to avoid. A number here has to
  // mean "somebody asked you something".
  //
  // Polls rather than subscribes: the SSE marker stream is not wired
  // to this shell, and 30s is well inside the latency that matters for
  // a question waiting in a queue. `?kind=direct` is counted
  // server-side, so this is one small JSON response, not the inbox.
  let unreadDirect = $state(0);

  $effect(() => {
    const id = user?.id;
    if (!id) return;
    // Bound to a local so the narrowing survives into the closure —
    // `user?.id` is `string | undefined` and TS re-widens it there.
    const uid: string = id;
    let cancelled = false;
    async function poll() {
      try {
        const r = await fetch(
          `/api/messages/unread/${encodeURIComponent(uid)}?kind=direct`,
        );
        if (!r.ok) return;
        const body = (await r.json()) as { count?: number };
        if (!cancelled) unreadDirect = body.count ?? 0;
      } catch {
        // A failed poll leaves the last known count rather than
        // zeroing it: showing "nothing waiting" because the network
        // blinked is the one wrong answer this badge can give.
      }
    }
    void poll();
    const t = setInterval(poll, 30_000);
    return () => {
      cancelled = true;
      clearInterval(t);
    };
  });

  // A department's sidebar is DERIVED (car 2 of design 8c3e9599, backlog
  // 64656a46): `departmentRows` — its own catalog surfaces in catalog
  // order, then its Jobs row — so a department the registry adds gets
  // its sidebar with no edit here. Two hand lists answered this until
  // then: APP_SURFACES, each department's surface order, and IT_GROUPS,
  // IT's seven rows. Both repeated what the catalog already held, and
  // APP_SURFACES disagreed with it: it listed Sales' Accounts first while
  // the tab opened on the pipeline. `visible()` then drops whatever the
  // role or the tenant manifest blocks.

  // The group header is the department's own label — the registry's
  // display name, because a second spelling of "Finance" is a second
  // thing to keep in step.
  function appGroupLabel(app: AppId): string {
    return app === 'home' || app === 'simulator' ? '' : departmentLabel(app, departments());
  }

  // Work group is All jobs, for every role (backlog 0f9be7c0,
  // 2026-09-24). It was role-keyed — a closed map until 6a3b93eb, then
  // each role row's `metadata.work` — but visible() drops every entry
  // whose catalog owner is not home, so of any list only `jobs` could
  // render and a list without it left Work empty. Each department app
  // has its own sidebar; a role's surfaces are gated there, by the
  // row's `surfaces`, and Work here is the same filter over one row.
  const WORK: NavGroup = {
    label: 'Work',
    items: [ROUTE_CATALOG.jobs],
  };

  // Home — personal work, whichever domain it belongs to: "what am I
  // meant to be doing" is one question, and its answer (All jobs, My
  // Day) crosses every department freely.
  //
  // Mine lists Home surfaces only. It carried Exec until backlog
  // e8fe5e5a (2026-09-24), a row visible() dropped for every role —
  // Exec's catalog owner is executive — so it never rendered here; it is
  // the Executive department's row, under a tab every role is offered.
  const HOME_GROUPS: ReadonlyArray<NavGroup> = [
    WORK,
    {
      label: 'Mine',
      items: [
        // Permkey-less: /` is App.svelte's `me` route, which has no
        // catalog entry (it is everyone's, ungated).
        { id: 'my-day', label: 'My Day', path: '/' },
        ROUTE_CATALOG.inbox,
        ROUTE_CATALOG.views,
        ROUTE_CATALOG.schedule,
        ROUTE_CATALOG['company-map'],
      ],
    },
  ];

  // Every department group ends on its Jobs row — the department's in
  // / working / out over the packets whose workflow declares it
  // (cc76f755, 2026-09-18) — IT's included, since car 2 of design
  // 8c3e9599: every unit carries it. `departmentRows` says why it is
  // permKey-less.
  let MAIN = $derived<ReadonlyArray<NavGroup>>(
    activeApp === 'home'
      ? HOME_GROUPS
      : [{ label: appGroupLabel(activeApp), items: departmentRows(activeApp) }],
  );

  // A surface is in-perspective when its own catalog `owner` matches the
  // app this shell is rendering — inPerspective in ./nav-catalog, where
  // the test pinning every sidebar list imports it (72a88031). One
  // comparison against one field, where this used to be a MODEL_ROUTES
  // set here that had to agree with a MODEL_KINDS set in App.svelte.
  function visible(items: ReadonlyArray<NavItem>): ReadonlyArray<NavItem> {
    if (!role) return [];
    return items.filter((i) => {
      const policyOk = i.permKey === undefined || canSeeRoute(role, i.permKey, roleRow);
      const moduleOk = i.module === undefined || moduleEnabled(i.module);
      return policyOk && moduleOk && inPerspective(i, activeApp);
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
            {#if unreadDirect > 0}
              <span
                class="shell-nav-badge"
                title="{unreadDirect} message{unreadDirect === 1 ? '' : 's'} addressed to you"
                aria-label="{unreadDirect} unread message{unreadDirect === 1 ? '' : 's'} addressed to you"
              >{unreadDirect}</span>
            {/if}
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
                href={safeLinkHref(item.path)}
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
        <!-- The name is the door to the profile — passkeys, assignments,
             the person's own page (feedback 16414d99: "Let's have the
             user's name in the bottom right click into the profile"). -->
        <a
          class="shell-user"
          href={href('/ux/me')}
          title="Your profile and passkeys"
          onclick={(e) => {
            e.preventDefault();
            navigate('/ux/me');
          }}
        >
          <div class="shell-user-name">{user.name}</div>
          <div class="shell-user-role">{user.role}</div>
        </a>
      {:else if session.value.kind === 'unresolved'}
        <!-- Signed in, but the viewer's own people row did not answer.
             The chrome says so on every page rather than rendering
             nobody (backlog b4f68a65). -->
        <p class="load-failed" role="alert" style="font-size:12px">
          Couldn't load your employee record — {session.value.error}.
        </p>
      {/if}
      <ClassesReadFailed subjectKind="employee" what="roles" fallback="The sidebar shows every surface, not the ones your role declares." style="font-size:12px" />
    </div>
  </aside>

  <div class="shell-main">
    <!-- Demo-mode persona switcher — fixed-positioned (bottom-left),
         so it renders here but floats independently of the layout.
         The system-time + sign-in chrome moved up to the perspective
         tab bar; the old topbar is gone. -->
    <div class="shell-content">
      {@render children()}
    </div>
  </div>
</div>
