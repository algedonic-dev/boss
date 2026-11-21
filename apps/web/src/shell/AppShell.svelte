<script lang="ts">
  // App shell — persistent sidebar + content slot.
  //
  // THE SIDEBAR IS WHAT THE STATION BEING VIEWED HOSTS (backlog 474e2c1c,
  // David 2026-11-19: "I want the sidebar of a station to hold whatever
  // navigation links or internal pages to the station that the station
  // wants to host"). Its rows are the catalog entries whose `host` is that
  // station (shell/station-trail.ts `sidebarOf`), and on the person's own
  // pages, the person's own. Moving between stations is the top bar's job
  // (StationTrail.svelte), so no row here names another station.
  //
  // It was a department's rows until then — the entries a
  // departments-registry code owned, then a Jobs row — under one tab per
  // row of that registry, so both bars drew departments the company no
  // longer has (David 2026-11-20). The department rows and the hand-kept Home
  // group retired with it. The visible() filter still drops whatever the
  // role or the tenant manifest blocks.

  import ClassesReadFailed from '@boss/web-kit/ui/ClassesReadFailed.svelte';
  import { session } from '@boss/web-kit/session/session.svelte';
  import { moduleEnabled, getLabel } from '@boss/web-kit/session/manifest.svelte';
  import { canSeeRoute, type Role } from '@boss/web-kit/session/permissions';
  import { href, navigate } from '../router';
  import type { NavItem } from './nav-catalog';
  import { pagePath } from './page-address';
  import { stationAddresses } from './stationAddresses.svelte';
  import { sidebarOf, type Viewing } from './station-trail';
  import { classesFor } from '@boss/web-kit/session/classes.svelte';
  import { safeLinkHref } from '@boss/web-kit/links';

  let { activeSection, viewing, children } = $props<{
    activeSection: string;
    /// What the page views (station-trail.ts `viewedBy`), the same value
    /// the top bar draws the path to.
    viewing: Viewing;
    children: () => any;
  }>();

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

  let sidebar = $derived(sidebarOf(viewing as Viewing, stationAddresses()));

  function visible(items: ReadonlyArray<NavItem>): ReadonlyArray<NavItem> {
    if (!role) return [];
    return items.filter((i) => {
      const policyOk = i.permKey === undefined || canSeeRoute(role, i.permKey, roleRow);
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
    <nav class="shell-nav" data-sidebar-state={sidebar.kind}>
      {#if sidebar.kind === 'loading'}
        <p class="shell-nav-note" role="status">Reading the station tree…</p>
      {:else if sidebar.kind === 'failed'}
        <!-- Said, never an empty sidebar: no rows would read as a station
             that hosts nothing (builder rule 21.2). Its own class, not
             `load-failed`: that one marks a PAGE's primary read, and the
             chrome wearing it too made two on the station floor's failed
             read (gate-run 45fa3237, station-tree-floor.mocked.spec.ts). -->
        <p class="shell-nav-note shell-nav-failed" role="alert" data-sidebar-failed>Couldn't read the station tree — {sidebar.why}</p>
      {:else}
        {@const items = visible(sidebar.items)}
        {#if items.length > 0 || (sidebar.label !== '' && role !== null)}
          <div class="shell-nav-group" data-sidebar-station={sidebar.label || undefined}>
            {#if sidebar.label !== ''}
              <div class="shell-nav-group-label">
                <span class="shell-nav-group-chevron">▾</span>
                {sidebar.label}
              </div>
            {/if}
            {#if items.length === 0}
              <!-- A station that hosts no page you can open says so: an
                   empty group would read as a sidebar that failed. -->
              <p class="shell-nav-note" data-sidebar-empty>No pages hosted here.</p>
            {/if}
            {#each items as item (item.id)}
              <!-- A hosted page's path is its host station's address in
                   the tree plus its slug (backlog 0bc2eb68): until the
                   tree answers, the row names the page and links nowhere. -->
              {@const to = pagePath(item, stationAddresses())}
              {#if to === null}
                <span class="shell-nav-item" data-unplaced>{getLabel(`nav.${item.id}_label`, item.label)}</span>
              {:else}
                <a
                  href={safeLinkHref(to)}
                  class="shell-nav-item {activeSection === item.id ? 'shell-nav-item-active' : ''}"
                  onclick={(e) => onLinkClick(e, to)}
                >
                  {getLabel(`nav.${item.id}_label`, item.label)}
                  {#if item.id === 'inbox' && unreadDirect > 0}
                    <span
                      class="shell-nav-badge"
                      title="{unreadDirect} message{unreadDirect === 1 ? '' : 's'} addressed to you"
                      aria-label="{unreadDirect} unread message{unreadDirect === 1 ? '' : 's'} addressed to you"
                    >{unreadDirect}</span>
                  {/if}
                </a>
              {/if}
            {/each}
          </div>
        {/if}
      {/if}
    </nav>



    <div class="shell-sidebar-footer">
      {#if user}
        <!-- The name is the door to the profile — passkeys, assignments,
             the person's own page (feedback 16414d99: "Let's have the
             user's name in the bottom right click into the profile"). -->
        <a
          class="shell-user"
          href={href('/me')}
          title="Your profile and passkeys"
          onclick={(e) => {
            e.preventDefault();
            navigate('/me');
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
