<script lang="ts">
  // /system/fleet — every in-flight Job of one Workflow kind,
  // projected onto the Workflow's DAG.
  //
  // The Job page answers "where is THIS Job"; this page answers
  // "where is EVERYTHING of this kind": per-step depth (ready /
  // active), the unassigned claimable pool, which authority-role lens
  // each pile belongs to, and how long the oldest wait has run. A hot
  // node is a deep queue — the algedonic depth signal from
  // queue-visibility Q4 drawn on the map (feedback 9fe2fe66,
  // change 1; thresholds/telemetry are change 2, gated on Q4).
  //
  // Polls on a 10s interval, per the SSE policy's bucket (b): depth
  // is an aggregate a single event does not unambiguously update, so
  // it re-fetches rather than streaming.
  //
  // Steps that do not match the current spec's slugs — pre-migration
  // slug-less rows grouped by title, or steps from older Workflow
  // versions — render in the off-map table below the DAG rather than
  // silently vanishing (the server's COALESCE contract; see
  // boss-views/src/fleet.rs).
  import { onMount } from 'svelte';
  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import StepDag, { type DagNode } from '../../jobs/StepDag.svelte';
  import { workflowToDag } from '../../jobs/workflowToDag';

  type FleetNode = Readonly<{
    slug: string;
    ready: number;
    active: number;
    unassigned: number;
    by_role: Readonly<Record<string, number>>;
    oldest_ready_wall: string | null;
  }>;
  type Fleet = Readonly<{
    workflow_kind: string;
    open_jobs: number;
    nodes: ReadonlyArray<FleetNode>;
    as_of: string;
  }>;
  type SpecStep = Readonly<{
    title: string;
    kind: string;
    ready_when?: string;
    title_template?: string | null;
    terminal?: { outcome: string } | null;
  }>;

  const POLL_MS = 10_000;

  let kinds = $state<ReadonlyArray<string>>([]);
  let kind = $state<string | null>(null);
  let specSteps = $state<ReadonlyArray<SpecStep> | null>(null);
  let fleet = $state<Fleet | null>(null);
  let loading = $state(true);
  let error = $state<string | null>(null);

  async function loadKinds(): Promise<void> {
    const res = await fetch('/api/workflows');
    if (!res.ok) throw new Error(`workflows: HTTP ${res.status}`);
    const rows: ReadonlyArray<{ kind: string }> = await res.json();
    kinds = [...new Set(rows.map((r) => r.kind))].sort();
    // Deep-linkable: /system/fleet?kind=wholesale-keg-order.
    const asked = new URLSearchParams(window.location.search).get('kind');
    kind = asked && kinds.includes(asked) ? asked : (kinds[0] ?? null);
  }

  async function loadSpec(k: string): Promise<void> {
    const res = await fetch(`/api/workflows/${encodeURIComponent(k)}`);
    if (!res.ok) throw new Error(`workflow ${k}: HTTP ${res.status}`);
    const spec = await res.json();
    specSteps = (spec.steps ?? []) as ReadonlyArray<SpecStep>;
  }

  async function loadFleet(k: string): Promise<void> {
    const res = await fetch(`/api/views/fleet/${encodeURIComponent(k)}`);
    if (!res.ok) throw new Error(`fleet ${k}: HTTP ${res.status}`);
    fleet = (await res.json()) as Fleet;
  }

  async function switchTo(k: string): Promise<void> {
    kind = k;
    loading = true;
    error = null;
    try {
      await Promise.all([loadSpec(k), loadFleet(k)]);
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      loading = false;
    }
  }

  onMount(() => {
    void (async () => {
      try {
        await loadKinds();
        if (kind) await switchTo(kind);
        else {
          loading = false;
          error = 'no Workflows in the registry';
        }
      } catch (e) {
        loading = false;
        error = e instanceof Error ? e.message : String(e);
      }
    })();
    const timer = setInterval(() => {
      if (kind) void loadFleet(kind).catch(() => {});
    }, POLL_MS);
    return () => clearInterval(timer);
  });

  function badge(n: FleetNode): string {
    const parts: string[] = [];
    if (n.ready > 0) parts.push(`${n.ready} ready`);
    if (n.active > 0) parts.push(`${n.active} active`);
    if (n.unassigned > 0) parts.push(`${n.unassigned} unclaimed`);
    return parts.join(' · ');
  }

  /// Wall-clock age of the oldest still-ready step, against the
  /// server's clock — never the browser's, never sim time.
  function age(n: FleetNode): string {
    if (!n.oldest_ready_wall || !fleet) return '—';
    const ms = Date.parse(fleet.as_of) - Date.parse(n.oldest_ready_wall);
    if (ms < 0) return '—';
    const h = ms / 3_600_000;
    if (h < 1) return `${Math.round(h * 60)}m`;
    if (h < 48) return `${h.toFixed(1)}h`;
    return `${(h / 24).toFixed(1)}d`;
  }

  function roles(n: FleetNode): string {
    const entries = Object.entries(n.by_role);
    if (entries.length === 0) return '—';
    return entries.map(([r, c]) => `${r}: ${c}`).join(', ');
  }

  let bySlug = $derived(new Map((fleet?.nodes ?? []).map((n) => [n.slug, n])));

  /// The spec's DAG with fleet depth decorated on. A node with active
  /// work lights up active; ready-only lights up ready; idle stays
  /// neutral — the DAG reuses the step-status visual language for the
  /// fleet's aggregate state.
  let dag = $derived.by(() => {
    if (!specSteps) return null;
    const { nodes, edges } = workflowToDag(specSteps);
    const decorated: DagNode[] = nodes.map((n) => {
      const f = bySlug.get(n.id);
      if (!f) return n;
      return {
        ...n,
        status: f.active > 0 ? 'active' : f.ready > 0 ? 'ready' : undefined,
        badge: badge(f) || null,
      };
    });
    return { nodes: decorated, edges };
  });

  /// Fleet groups with no home on the current spec's DAG: slug-less
  /// steps grouped by title, and steps of superseded versions whose
  /// slugs the current version dropped.
  let offMap = $derived.by(() => {
    if (!fleet) return [];
    const onMap = new Set((dag?.nodes ?? []).map((n) => n.id));
    return fleet.nodes.filter((n) => !onMap.has(n.slug));
  });

  let inFlight = $derived(
    (fleet?.nodes ?? []).reduce((sum, n) => sum + n.ready + n.active, 0),
  );
</script>

<PageHeader
  title="Fleet"
  subtitle="Every in-flight Job of a kind, projected onto its Workflow"
/>

<div class="fleet-bar">
  <label class="fleet-pick">
    Workflow
    <select
      value={kind ?? ''}
      onchange={(e) => void switchTo((e.target as HTMLSelectElement).value)}
    >
      {#each kinds as k (k)}
        <option value={k}>{k}</option>
      {/each}
    </select>
  </label>
  {#if fleet}
    <span class="fleet-scope">
      {fleet.open_jobs} open · {inFlight} steps in flight · as of {fleet.as_of}
    </span>
  {/if}
</div>

{#if loading}
  <p class="fleet-msg">Reading the fleet…</p>
{:else if error}
  <p class="fleet-msg fleet-err">{error}</p>
{:else if dag}
  <StepDag nodes={dag.nodes} edges={dag.edges} />

  {#if fleet && fleet.nodes.length > 0}
    <table class="fleet-table">
      <thead>
        <tr>
          <th>Step</th>
          <th>Ready</th>
          <th>Active</th>
          <th>Unclaimed</th>
          <th>Role lenses</th>
          <th>Oldest wait</th>
        </tr>
      </thead>
      <tbody>
        {#each fleet.nodes as n (n.slug)}
          <tr>
            <td>
              {n.slug}
              {#if offMap.includes(n)}
                <span class="fleet-offmap" title="No matching step on the current Workflow version — a slug-less step grouped by title, or a step of a superseded version">off map</span>
              {/if}
            </td>
            <td>{n.ready}</td>
            <td>{n.active}</td>
            <td>{n.unassigned}</td>
            <td>{roles(n)}</td>
            <td>{age(n)}</td>
          </tr>
        {/each}
      </tbody>
    </table>
  {:else}
    <p class="fleet-msg">Nothing in flight for this kind.</p>
  {/if}
{/if}

<style>
  .fleet-bar {
    display: flex;
    align-items: baseline;
    gap: 16px;
    margin: 12px 0;
  }
  .fleet-pick {
    display: inline-flex;
    align-items: baseline;
    gap: 8px;
    font-size: 13px;
    color: var(--color-fg-muted, #8a7a5f);
  }
  .fleet-scope {
    font-size: 12px;
    color: var(--color-fg-muted, #8a7a5f);
  }
  .fleet-msg {
    margin: 24px 0;
    color: var(--color-fg-muted, #8a7a5f);
  }
  .fleet-err {
    color: var(--color-danger, #a33);
  }
  .fleet-table {
    margin-top: 16px;
    border-collapse: collapse;
    font-size: 13px;
  }
  .fleet-table th,
  .fleet-table td {
    text-align: left;
    padding: 6px 14px 6px 0;
    border-bottom: 1px solid var(--color-border, #e4dccb);
  }
  .fleet-table th {
    font-weight: 600;
    color: var(--color-fg-muted, #8a7a5f);
  }
  .fleet-offmap {
    margin-left: 6px;
    font-size: 11px;
    font-weight: 600;
    color: var(--color-accent, #7a3f1f);
  }
</style>
