<script lang="ts">
  // The IT family tab strip (2026-08-31 consolidation, 1f6d55e0):
  // Operate / Registry / Design each carry their family as tabs on
  // one surface instead of sidebar rows. One component, data-driven —
  // adding a tab is a row here, not a new sidebar entry (the sidebar
  // holds exactly six IT rows and this file is why it can).
  //
  // Each tab names its page by CATALOG ID, never a path (backlog
  // 0bc2eb68): a page's path is its host station's address in the tree
  // plus its slug, so the strip asks the catalog where each page is now.
  import { href, navigate } from '../router';
  import { safeLinkHref } from '@boss/web-kit/links';
  import { pageHref } from '../shell/stationAddresses.svelte';
  import type { ROUTE_CATALOG } from '../shell/nav-catalog';

  export type ItTabGroup = 'operate' | 'registry' | 'design';
  type PageId = keyof typeof ROUTE_CATALOG;

  const GROUPS: Readonly<Record<ItTabGroup, ReadonlyArray<{ label: string; page: PageId }>>> = {
    // Car N3 of design e765b3fc (2026-09-25) took five tabs off this
    // strip, each now a selection's panel on the Department Map: Yard
    // status (the track, dock and garage stations), Conductor (the
    // track), Receiving Yard and Marshalling Yard (those two stations),
    // and on Design, Feedback and Backlog (receiving; the backlog on
    // marshalling too). What stays is desk work, not a station.
    // Incidents, Atlas, Bottlenecks, Policy and Experiments went with
    // step 1 of design 01fa5bdf (David, 2026-11-17).
    operate: [
      { label: 'Audit Log', page: 'system-monitoring' },
      { label: 'Performance', page: 'system-performance' },
    ],
    registry: [
      { label: 'Workflows', page: 'workflows' },
      { label: 'Dispatcher', page: 'system-dispatcher' },
      // The authoring list of the rules the cascade draws. Rendered bare
      // until 2026-09-24, reachable only through the cascade's "Edit
      // rules →" link, and 0 of 784 surface-opens reached it (0a98d93f).
      { label: 'Rules', page: 'system-dispatcher-rules' },
      { label: 'Step plugins', page: 'system-step-plugins' },
      { label: 'Subjects', page: 'system-subjects' },
      { label: 'Drift', page: 'system-registry-drift' },
      // The agents registry as a directory (62988516) — who runs work
      // besides people, beside the other registries.
      { label: 'Agents', page: 'system-agents' },
      // The credentials registry, its lifecycle as packets (851259b9;
      // design 76155676 step 3) — what the estate holds access with.
      { label: 'Credentials', page: 'system-credentials' },
    ],
    design: [
      { label: 'Reviews', page: 'system-design' },
    ],
  };

  let { group, active }: { group: ItTabGroup; active: PageId } = $props();
  const tabs = GROUPS[group];
</script>

<nav class="it-tabs" aria-label="IT {group}">
  {#each tabs as t (t.page)}
    {@const to = pageHref(t.page)}
    {#if to !== null}
      <a
        href={safeLinkHref(href(to))}
        class:active={t.page === active}
        aria-current={t.page === active ? 'page' : undefined}
        onclick={(e) => {
          e.preventDefault();
          navigate(to);
        }}>{t.label}</a
      >
    {/if}
  {/each}
</nav>

<style>
  .it-tabs {
    display: flex;
    gap: 0.25rem;
    padding: 0.5rem 1.25rem 0;
    border-bottom: 1px solid var(--border);
    flex-wrap: wrap;
  }
  .it-tabs a {
    padding: 0.35rem 0.75rem;
    border: 1px solid transparent;
    border-bottom: none;
    border-radius: 6px 6px 0 0;
    color: inherit;
    text-decoration: none;
    font-size: 0.85rem;
  }
  .it-tabs a:hover {
    background: var(--ink-raised);
  }
  .it-tabs a.active {
    border-color: var(--border);
    background: var(--ink);
    font-weight: 600;
  }
</style>
