<script lang="ts">
  // The IT flow network — job flow through IT's queues, on the page
  // the team already reads (feedback 39d5bfde; department-flow-
  // dashboards Q2+Q4 as David decided them: node-set = IT's kinds,
  // absorbed into /system/flow).
  //
  // Composition, not invention: each IT Workflow kind renders as its
  // decorated DAG (depth badges from /api/views/fleet, hop latency
  // from /api/views/stage-durations — the shared decorateDag
  // grammar), sections ordered by the pipeline the job_edges
  // registry declares, with link bars between kinds showing the
  // declared field and how many open Jobs carry it. Node deep-dive
  // is the Bottlenecks page — this view answers "where is the
  // department's work moving", not "which item".
  //
  // Deliberately deferred: representative external-actor nodes (the
  // second half of 39d5bfde) and edge pulses (dashboards Q3) — both
  // noted on the item.
  import StepDag from '../../jobs/StepDag.svelte';
  import { workflowToDag } from '../../jobs/workflowToDag';
  import { decorateDagNodes, type FleetNodeStat, type StageStat } from '../../jobs/decorateDag';
  import { navigate } from '../../router';

  type EdgeSpec = Readonly<{
    source_kind: string;
    field_path: string;
    field_kind: string;
  }>;
  type KindData = Readonly<{
    kind: string;
    nodes: ReturnType<typeof workflowToDag>['nodes'];
    edges: ReturnType<typeof workflowToDag>['edges'];
    openJobs: number;
    linkCounts: ReadonlyMap<string, number>;
  }>;

  let kinds = $state<ReadonlyArray<string>>([]);
  let edgeSpecs = $state<ReadonlyArray<EdgeSpec>>([]);
  let sections = $state<ReadonlyArray<KindData>>([]);
  let loading = $state(true);
  let error = $state<string | null>(null);

  async function jsonOr<T>(url: string, fallback: T): Promise<T> {
    try {
      const r = await fetch(url);
      if (!r.ok) return fallback;
      return (await r.json()) as T;
    } catch {
      return fallback;
    }
  }

  async function load(background = false): Promise<void> {
    if (!background) loading = true;
    try {
      // The Flow endpoint already knows the team's kinds (owner_role
      // over the registry — no list in code, CLAUDE.md §9).
      const flow = await jsonOr<{ kinds?: string[] }>('/api/views/flow?limit=1', {});
      const teamKinds = Array.isArray(flow.kinds) ? flow.kinds : [];
      const declared = await jsonOr<EdgeSpec[]>('/api/jobs/job-edges', []);
      edgeSpecs = Array.isArray(declared) ? declared : [];
      // The pipeline can reference kinds outside the owner set
      // (pr-train is the conductor's, not feedback's) — the network
      // includes any kind a declared edge touches.
      const linked = new Set(teamKinds);
      for (const e of edgeSpecs) linked.add(e.source_kind);
      kinds = [...linked].sort();

      const perKind = await Promise.all(
        kinds.map(async (kind) => {
          const [spec, fleet, stages, jobs] = await Promise.all([
            jsonOr<{ steps?: unknown }>(`/api/workflows/${encodeURIComponent(kind)}`, {}),
            jsonOr<{ open_jobs?: number; nodes?: FleetNodeStat[] }>(
              `/api/views/fleet/${encodeURIComponent(kind)}`,
              {},
            ),
            jsonOr<{ stages?: StageStat[] }>(
              `/api/views/stage-durations/${encodeURIComponent(kind)}?days=7`,
              {},
            ),
            jsonOr<{ data?: { metadata?: Record<string, unknown> }[] }>(
              `/api/jobs?kind=${encodeURIComponent(kind)}&status=open&limit=200`,
              {},
            ),
          ]);
          const steps = Array.isArray(spec.steps) ? spec.steps : [];
          const { nodes, edges } = workflowToDag(steps as never);
          const decorated = decorateDagNodes(
            nodes,
            Array.isArray(fleet.nodes) ? fleet.nodes : [],
            Array.isArray(stages.stages) ? stages.stages : [],
          );
          // How many open Jobs of this kind carry each declared link.
          const linkCounts = new Map<string, number>();
          const rows = Array.isArray(jobs.data) ? jobs.data : [];
          for (const e of edgeSpecs) {
            if (e.source_kind !== kind) continue;
            const n = rows.filter((j) => {
              const v = j.metadata?.[e.field_path];
              return e.field_kind === 'job_id_list'
                ? Array.isArray(v) && v.length > 0
                : typeof v === 'string' && v !== '';
            }).length;
            linkCounts.set(e.field_path, n);
          }
          return {
            kind,
            nodes: decorated,
            edges,
            openJobs: typeof fleet.open_jobs === 'number' ? fleet.open_jobs : 0,
            linkCounts,
          };
        }),
      );

      // Pipeline order from the declared edges: a kind whose edges
      // point AT another kind's Jobs sits downstream of feeders
      // (backlog_item points backward at the origin item; train
      // points forward at the train). Order: kinds nobody links into
      // first, link-target kinds after their sources — approximated
      // by: feedback-ish kinds (no outgoing declared edges) first,
      // then kinds with backward links, then pure targets.
      const hasOutgoing = new Set(edgeSpecs.map((e) => e.source_kind));
      perKind.sort((a, b) => {
        const rank = (k: KindData) =>
          !hasOutgoing.has(k.kind) && k.kind !== 'pr-train' ? 0 : k.kind === 'pr-train' ? 2 : 1;
        return rank(a) - rank(b) || a.kind.localeCompare(b.kind);
      });
      sections = perKind;
      error = null;
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      loading = false;
    }
  }

  $effect(() => {
    void load();
    const t = setInterval(() => void load(true), 20_000);
    return () => clearInterval(t);
  });

  function linksOutOf(kind: string): { field: string; count: number }[] {
    const section = sections.find((s) => s.kind === kind);
    if (!section) return [];
    return edgeSpecs
      .filter((e) => e.source_kind === kind)
      .map((e) => ({ field: e.field_path, count: section.linkCounts.get(e.field_path) ?? 0 }));
  }
</script>

<section class="fn-wrap">
  <h3 class="fn-h">
    The department's network
    <span class="fn-sub">
      job flow through IT's queues — depth and hop latency per step; click a kind for its Bottlenecks view
    </span>
  </h3>

  {#if loading}
    <p class="fn-msg">Reading the network…</p>
  {:else if error}
    <p class="fn-msg fn-err">{error}</p>
  {:else}
    {#each sections as sec, i (sec.kind)}
      {#if i > 0}
        {@const prev = sections[i - 1]}
        {@const links = (prev ? linksOutOf(prev.kind) : []).concat(linksOutOf(sec.kind))}
        {#if links.length > 0}
          <div class="fn-link-bar" role="presentation">
            {#each links as l (l.field)}
              <span class="fn-link">⇅ {l.field} · {l.count} open carrying it</span>
            {/each}
          </div>
        {/if}
      {/if}
      <div class="fn-kind">
        <button type="button" class="fn-kind-h" onclick={() => navigate(`/system/fleet?kind=${encodeURIComponent(sec.kind)}`)}>
          {sec.kind}
          <span class="fn-kind-n">{sec.openJobs} open</span>
        </button>
        <StepDag nodes={sec.nodes} edges={sec.edges} />
      </div>
    {/each}
  {/if}
</section>

<style>
  .fn-wrap {
    margin: 8px 0 24px;
  }
  .fn-h {
    font-size: 15px;
    display: flex;
    align-items: baseline;
    gap: 10px;
    flex-wrap: wrap;
  }
  .fn-sub {
    font-size: 12px;
    font-weight: 400;
    color: var(--color-fg-muted, #8a7a5f);
  }
  .fn-msg {
    margin: 12px 0;
    color: var(--color-fg-muted, #8a7a5f);
  }
  .fn-err {
    color: var(--color-danger, #a33);
  }
  .fn-kind {
    margin-top: 10px;
  }
  .fn-kind-h {
    font: inherit;
    font-size: 13px;
    font-weight: 600;
    display: flex;
    align-items: baseline;
    gap: 8px;
    background: transparent;
    border: none;
    cursor: pointer;
    color: inherit;
    padding: 0;
  }
  .fn-kind-h:hover {
    color: var(--color-accent, #7a3f1f);
  }
  .fn-kind-n {
    font-size: 12px;
    font-weight: 400;
    color: var(--color-fg-muted, #8a7a5f);
  }
  .fn-link-bar {
    margin: 12px 0 4px;
    display: flex;
    gap: 14px;
    flex-wrap: wrap;
  }
  .fn-link {
    font-size: 12px;
    font-weight: 600;
    color: var(--color-accent, #7a3f1f);
  }
</style>
