<script lang="ts">
  import { safeLinkHref } from '@boss/web-kit/links';
  // /admin/step-plugins — port of apps/web/src/admin/StepPluginsPage.tsx.

  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import Section from '@boss/web-kit/ui/Section.svelte';
  import Link from '@boss/web-kit/ui/Link.svelte';
  import type { StepPluginSpec } from './stepPluginTypes';
  import { href } from '../../router';
  import { readBindings, readCount, type Binding, type Usage } from './pluginUsage';
  import { STEP_PLUGIN_README_URL } from '../../public-source';

  let plugins = $state<ReadonlyArray<StepPluginSpec>>([]);
  let loading = $state(true);
  let error = $state<string | null>(null);

  let bindings = $state<Usage<ReadonlyArray<Binding>>>({kind: 'loading'});
  let counts = $state<Readonly<Record<string, Usage<number>>>>({});

  async function loadBindings(): Promise<void> {
    try {
      const r = await fetch('/api/workflows');
      if (!r.ok) throw new Error(`HTTP ${r.status}: ${await r.text()}`);
      bindings = {kind: 'known', value: readBindings(await r.json())};
    } catch (e) {
      bindings = {kind: 'unknown', reason: e instanceof Error ? e.message : String(e)};
    }
  }

  async function loadCount(kind: string): Promise<void> {
    let answer: Usage<number>;
    try {
      const r = await fetch(`/api/jobs/step-plugins/${encodeURIComponent(kind)}/in-flight-count`);
      if (!r.ok) throw new Error(`HTTP ${r.status}: ${await r.text()}`);
      answer = {kind: 'known', value: readCount(await r.json(), kind)};
    } catch (e) {
      answer = {kind: 'unknown', reason: e instanceof Error ? e.message : String(e)};
    }
    counts = {...counts, [kind]: answer};
  }

  async function load(): Promise<void> {
    loading = true;
    try {
      const r = await fetch('/api/jobs/step-plugins');
      if (!r.ok) throw new Error(`HTTP ${r.status}: ${await r.text()}`);
      plugins = (await r.json()) as StepPluginSpec[];
      error = null;
      counts = Object.fromEntries(plugins.map((p) => [p.kind, {kind: 'loading'}]));
      for (const p of plugins) void loadCount(p.kind);
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      loading = false;
    }
  }

  $effect(() => {
    void load();
    void loadBindings();
  });

  let byCategory = $derived.by(() => {
    const m = new Map<string, StepPluginSpec[]>();
    for (const p of plugins) {
      const arr = m.get(p.category) ?? [];
      arr.push(p);
      m.set(p.category, arr);
    }
    for (const [, arr] of m) arr.sort((a, b) => a.kind.localeCompare(b.kind));
    return m;
  });
  let categoryKeys = $derived([...byCategory.keys()].sort());
</script>

<div class="catalog theme-exec">
  <PageHeader
    eyebrow="Platform · Step plugins"
    title="Step UX plugins"
    subtitle={loading
      ? 'Loading…'
      : error
        ? // A failed read leaves `plugins` at [] — counting that would
          // paint the empty registry's "0 active plugins" beside the
          // failure line (backlog 044f55e4). The count is unknown, so say so.
          'Plugin count unknown — the registry read failed'
        : `${plugins.length} active plugin${plugins.length === 1 ? '' : 's'} across ${categoryKeys.length} categor${categoryKeys.length === 1 ? 'y' : 'ies'}`}
  />
  {#if error}
    <!-- load-failed + role=alert: the shared failure marker the outage
         crawl asserts (tests/mocked/_routes.ts FAILURE_MARKER; backlog 7267f9ce).
         No inline colour: the marker's own rule sets the words in troubled
         ink, and an inline --err outranked it (sweep c3e4edcc). -->
    <p class="empty load-failed" role="alert" style="margin:0 24px">Failed to load: {error}</p>
  {/if}

  {#if plugins.length === 0 && !loading && !error}
    <p class="empty" style="padding:0 24px">
      No plugins installed yet. Declare a row in
      <code class="mono">infra/platform/step-plugins/</code> and add its JavaScript bundle.
      Land the car; <code class="mono">boss-platform-workflow-seed</code> publishes the row on the next start.
      See <a href={safeLinkHref(STEP_PLUGIN_README_URL)} target="_blank" rel="noopener"><code class="mono">infra/step-plugins/README.md</code></a> for the shape and delivery steps.
    </p>
  {/if}

  <div class="tab-grid">
    {#each categoryKeys as cat (cat)}
      <Section title={cat} wide>
          <table class="data-table data-table-striped">
            <thead>
              <tr>
                <th>Kind</th>
                <th>Label</th>
                <th>Owner</th>
                <th class="num">Version</th>
                <th>Frontend bundle</th>
                <th>Active workflow steps</th>
                <th class="num">In flight (all packets)</th>
              </tr>
            </thead>
            <tbody>
              {#each byCategory.get(cat) ?? [] as p (p.kind)}
                {@const count = counts[p.kind]}
                <tr>
                  <td>
                    <Link to={href(`/it/registry/step-plugins/${encodeURIComponent(p.kind)}`)}>
                      <span class="mono">{p.kind}</span>
                    </Link>
                  </td>
                  <td>{p.label}</td>
                  <td>
                    {#if p.owning_team === 'platform'}
                      <span style="color:var(--static); font-size:12px">system</span>
                    {:else}
                      <span class="mono">{p.owning_team}</span>
                    {/if}
                  </td>
                  <td class="num">{p.version}</td>
                  <td><code class="mono" style="font-size:11px">{p.frontend_url}</code></td>
                  <td data-usage="bindings">
                    {#if bindings.kind === 'loading'}Loading…
                    {:else if bindings.kind === 'unknown'}<span title={bindings.reason}>Unknown</span>
                    {:else}
                      {@const matches = bindings.value.filter((b) => b.kind === p.kind)}
                      {#each matches as b (`${b.workflow}/${b.step}`)}
                        <div><Link to={href(`/it/registry/${encodeURIComponent(b.workflow)}`)}>{b.workflow} v{b.version} · {b.step}</Link></div>
                      {:else}None in active workflows{/each}
                    {/if}
                  </td>
                  <td class="num" data-usage="in-flight">
                    {#if !count || count.kind === 'loading'}Loading…
                    {:else if count.kind === 'unknown'}<span title={count.reason}>Unknown</span>
                    {:else}{count.value}{/if}
                  </td>
                </tr>
              {/each}
            </tbody>
          </table>
      </Section>
    {/each}
  </div>
</div>
