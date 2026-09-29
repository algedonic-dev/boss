<script lang="ts">
  // /it/operate/bottlenecks — every in-flight Job of one Workflow kind,
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
  // it re-fetches rather than streaming. A failed poll keeps the last
  // good numbers under a stale line naming the read (0fb86007); an
  // answer for a kind no longer selected is dropped, and a tick waits
  // for the last one to come back (44a8b680).
  //
  // It opens on the kind holding the most in-flight steps, its picker
  // sorted by that count (f949d19a), and every choice in the picker
  // writes ?kind= so a chosen kind can be reloaded and linked to — the Department
  // Map's sidings link here (c7c5c1de).
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
  import { groupByPosition } from '../../jobs/position';
  import type { ListedJob } from '../../jobs/types';
  import { navigate } from '../../router';
  import { fetchEvery, wholeOrThrow } from '../../data/paginated';
  import {
    failureText,
    fmtDur,
    inFlightByKind,
    openingKind,
    optionLabel,
    pickerOrder,
    searchWithKind,
    staleLine,
    type PolledRead,
  } from './fleet';

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
  type StageStat = Readonly<{
    slug: string;
    completed: number;
    p50_seconds: number;
    p90_seconds: number;
    max_seconds: number;
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
  // In-flight steps per kind, for the picker; `null` until read, and
  // stays null when the lens did not answer (`countsError` says so).
  let kindCounts = $state<ReadonlyMap<string, number> | null>(null);
  let countsError = $state<string | null>(null);
  let kind = $state<string | null>(null);
  let specSteps = $state<ReadonlyArray<SpecStep> | null>(null);
  let fleet = $state<Fleet | null>(null);
  // Slim rows (backlog ea80b5fd): a position reads no step metadata.
  let jobs = $state<ReadonlyArray<ListedJob>>([]);
  // `null` is UNREAD — the flow columns say so rather than painting a
  // zero (ffc3444f); `stagesError` names why.
  let stageStats = $state<ReadonlyArray<StageStat> | null>(null);
  let stagesError = $state<string | null>(null);
  // The polled reads' last failure, and when each last answered.
  let pollFailures = $state<Readonly<Partial<Record<PolledRead, string>>>>({});
  let lastGood = $state<Readonly<Partial<Record<PolledRead, number>>>>({});
  let now = $state(Date.now());
  let selectedNode = $state<string | null>(null);
  let loading = $state(true);
  let error = $state<string | null>(null);
  // An empty registry is an answer, not a failed read, so it is not
  // painted on the failure marker `error` wears (sweep c3e4edcc).
  let noKinds = $state(false);

  // Which selection an answer belongs to. Bumped by every switch; an
  // answer carrying an older number is for a kind no longer on the
  // page and is dropped (44a8b680).
  let generation = 0;
  // A tick waits while the last one is still out.
  let polling = false;

  const message = (e: unknown): string => (e instanceof Error ? e.message : String(e));

  /// A non-OK answer, thrown with the server's own words kept.
  async function refuse(what: string, res: Response): Promise<never> {
    throw new Error(failureText(what, res.status, await res.text().catch(() => '')));
  }

  // Decode once, defensively, at the fetch site — the route-smoke
  // crawl runs every page against an adversarial mock, and a
  // non-array where an array belongs must render as an empty state,
  // not a runtime crash.
  async function readKinds(): Promise<ReadonlyArray<string>> {
    const res = await fetch('/api/workflows');
    if (!res.ok) throw new Error(`workflows: HTTP ${res.status}`);
    const rows: unknown = await res.json();
    return Array.isArray(rows)
      ? [...new Set(rows.map((r) => r?.kind).filter((k): k is string => typeof k === 'string'))]
      : [];
  }

  // The picker's counts, from the queue-age lens — read once, for the
  // order the page opens in. A lens that does not answer leaves the
  // picker alphabetical and says so; it never fails the page.
  async function readCounts(): Promise<void> {
    try {
      const res = await fetch('/api/jobs/queue-age');
      if (!res.ok) await refuse('in-flight counts, /api/jobs/queue-age', res);
      kindCounts = inFlightByKind(await res.json());
    } catch (e) {
      countsError = message(e);
    }
  }

  async function readSpec(k: string): Promise<ReadonlyArray<SpecStep>> {
    const res = await fetch(`/api/workflows/${encodeURIComponent(k)}`);
    if (!res.ok) throw new Error(`workflow ${k}: HTTP ${res.status}`);
    const spec: unknown = await res.json();
    const steps = (spec as { steps?: unknown } | null)?.steps;
    return Array.isArray(steps) ? (steps as ReadonlyArray<SpecStep>) : [];
  }

  async function readFleet(k: string): Promise<Fleet> {
    const res = await fetch(`/api/views/fleet/${encodeURIComponent(k)}`);
    if (!res.ok) await refuse(`fleet ${k}`, res);
    const raw: unknown = await res.json();
    const f = (raw ?? {}) as Partial<Fleet> & { nodes?: unknown };
    return {
      workflow_kind: typeof f.workflow_kind === 'string' ? f.workflow_kind : k,
      open_jobs: typeof f.open_jobs === 'number' ? f.open_jobs : 0,
      nodes: Array.isArray(f.nodes) ? (f.nodes as Fleet['nodes']) : [],
      as_of: typeof f.as_of === 'string' ? f.as_of : '',
    };
  }

  // The node badges are server counts; the item list under a
  // clicked node comes from EVERY open packet of the kind — one page
  // of 200 until backlog b68a9dde, which the panel could only hedge
  // about. A read that stops short fails the page, naming how many of
  // how many it held; the panel still says "N of M" if the two
  // disagree.
  async function readJobs(k: string): Promise<ReadonlyArray<ListedJob>> {
    return wholeOrThrow(await fetchEvery<ListedJob>(`/api/jobs?kind=${encodeURIComponent(k)}&status=open`));
  }

  // The flow through the process, not just the live set: completed
  // hops + wall-clock latency per stage over the trailing week, from
  // /api/views/stage-durations. An empty station still shows the
  // process breathing. Enhancement, not dependency — a failed read
  // leaves the live depth standing — but not silent: until ffc3444f a
  // non-OK answer set the stages to [] and Done (7d) read 0 on every
  // row, "nothing drains", with the server's 503 sentence thrown away.
  async function readStages(k: string): Promise<ReadonlyArray<StageStat>> {
    const res = await fetch(`/api/views/stage-durations/${encodeURIComponent(k)}?days=7`);
    if (!res.ok) await refuse(`stage-durations ${k}`, res);
    const body: unknown = await res.json();
    const stages = (body as { stages?: unknown } | null)?.stages;
    return Array.isArray(stages) ? (stages as ReadonlyArray<StageStat>) : [];
  }

  // The address follows the choice, with replaceState: a kind is
  // not a place the back button steps through (/ux/jobs's rule,
  // f8027805), and App does not re-parse the route on it.
  function writeKind(k: string): void {
    const { pathname, search, hash } = window.location;
    const next = searchWithKind(search, k);
    if (next !== search) window.history.replaceState(window.history.state, '', pathname + next + hash);
  }

  /// The picker's choice: the address first, then the reads. The kind
  /// the page OPENS on is not written — an address without ?kind= means
  /// "the deepest kind", and a tab link lands where it pointed.
  function choose(k: string): void {
    writeKind(k);
    void switchTo(k);
  }

  async function switchTo(k: string): Promise<void> {
    const mine = ++generation;
    kind = k;
    loading = true;
    error = null;
    selectedNode = null;
    fleet = null;
    jobs = [];
    stageStats = null;
    stagesError = null;
    pollFailures = {};
    lastGood = {};
    try {
      const [spec, f, js, stages] = await Promise.all([
        readSpec(k),
        readFleet(k),
        readJobs(k),
        readStages(k).then(
          (s) => ({ ok: true as const, s }),
          (e: unknown) => ({ ok: false as const, e: message(e) }),
        ),
      ]);
      if (mine !== generation) return;
      const at = Date.now();
      specSteps = spec;
      fleet = f;
      jobs = js;
      if (stages.ok) stageStats = stages.s;
      else stagesError = stages.e;
      lastGood = stages.ok ? { fleet: at, items: at, flow: at } : { fleet: at, items: at };
    } catch (e) {
      if (mine === generation) error = message(e);
    } finally {
      if (mine === generation) loading = false;
    }
  }

  /// One tick: the three live reads for the kind on the page. The
  /// answers land only if that kind is still the selection; a read
  /// that fails keeps its last good value and is named on the stale
  /// line until it answers again (0fb86007, 44a8b680).
  async function poll(): Promise<void> {
    now = Date.now();
    const k = kind;
    if (!k || loading || polling) return;
    polling = true;
    const mine = generation;
    try {
      const [f, js, st] = await Promise.allSettled([readFleet(k), readJobs(k), readStages(k)]);
      if (mine !== generation) return;
      const at = Date.now();
      now = at;
      let failures: Partial<Record<PolledRead, string>> = { ...pollFailures };
      let good: Partial<Record<PolledRead, number>> = { ...lastGood };
      function settle<T>(read: PolledRead, r: PromiseSettledResult<T>, apply: (v: T) => void): void {
        if (r.status === 'fulfilled') {
          apply(r.value);
          good = { ...good, [read]: at };
          failures = Object.fromEntries(Object.entries(failures).filter(([name]) => name !== read));
        } else {
          failures = { ...failures, [read]: message(r.reason) };
        }
      }
      settle('fleet', f, (v) => (fleet = v));
      settle('items', js, (v) => (jobs = v));
      settle('flow', st, (v) => {
        stageStats = v;
        stagesError = null;
      });
      // A flow read that has never answered is UNREAD, not stale: its
      // failure line says so, with the newest reason.
      if (st.status === 'rejected' && stageStats === null) stagesError = message(st.reason);
      pollFailures = failures;
      lastGood = good;
    } finally {
      polling = false;
    }
  }

  onMount(() => {
    void (async () => {
      try {
        const [registry] = await Promise.all([readKinds(), readCounts()]);
        kinds = pickerOrder(registry, kindCounts);
        // Deep-linkable: /it/operate/bottlenecks?kind=backlog-item.
        const first = openingKind(kinds, new URLSearchParams(window.location.search).get('kind'));
        if (first) await switchTo(first);
        else {
          loading = false;
          noKinds = true;
        }
      } catch (e) {
        loading = false;
        error = message(e);
      }
    })();
    const timer = setInterval(() => void poll(), POLL_MS);
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
  let byNode = $derived(groupByPosition(jobs));
  let statBySlug = $derived(new Map((stageStats ?? []).map((s) => [s.slug, s])));
  let stale = $derived(staleLine(pollFailures, lastGood, now));

  let selectedJobs = $derived(selectedNode ? (byNode.get(selectedNode) ?? []) : []);
  let selectedServerCount = $derived.by(() => {
    if (!selectedNode) return 0;
    const server = bySlug.get(selectedNode);
    return server ? server.ready + server.active : selectedJobs.length;
  });

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
  title="Bottlenecks"
  subtitle="Where work piles up: per-step depth, completed flow, and wall-clock latency for a Workflow kind"
/>

<div class="fleet-bar">
  <label class="fleet-pick">
    Workflow
    <select
      value={kind ?? ''}
      onchange={(e) => choose((e.target as HTMLSelectElement).value)}
    >
      {#each kinds as k (k)}
        <option value={k}>{optionLabel(k, kindCounts)}</option>
      {/each}
    </select>
  </label>
  {#if fleet}
    <span class="fleet-scope">
      {fleet.open_jobs} open · {inFlight} steps in flight · as of {fleet.as_of}
    </span>
  {/if}
</div>

{#if countsError}
  <p class="fleet-msg fleet-note load-failed">
    The in-flight counts did not answer — {countsError}. The picker is alphabetical, with no counts.
  </p>
{/if}

{#if loading}
  <p class="fleet-msg">Reading the fleet…</p>
{:else if error}
  <p class="fleet-msg load-failed" role="alert">Couldn't read the fleet — {error}</p>
{:else if noKinds}
  <p class="fleet-msg">No Workflows in the registry.</p>
{:else if dag}
  {#if stale}
    <p class="fleet-msg fleet-note fleet-stale" role="status">{stale}</p>
  {/if}
  {#if stageStats === null}
    <p class="fleet-msg fleet-note load-failed">
      The flow read failed — {stagesError ?? 'no answer yet'}. Done (7d), p50 / max and Expected
      wait are unread, not zero; the depth beside them is live.
    </p>
  {/if}
  <StepDag
    nodes={dag.nodes}
    edges={dag.edges}
    selectedId={selectedNode}
    onNodeClick={(id) => (selectedNode = selectedNode === id ? null : id)}
  />

  {#if selectedNode}
    <section class="fleet-node-items">
      <h3 class="fleet-node-h">
        {selectedNode}
        <span class="fleet-node-n">
          {selectedJobs.length === selectedServerCount
            ? `${selectedJobs.length} here`
            : `${selectedJobs.length} of ${selectedServerCount} shown`}
        </span>
      </h3>
      {#if selectedJobs.length === 0}
        <p class="fleet-msg">Nothing visible at this step (the server counts steps the open packets read here do not show).</p>
      {:else}
        <ul class="fleet-items">
          {#each selectedJobs as j (j.id)}
            <li>
              <button type="button" class="fleet-item" onclick={() => navigate(`/jobs/${j.id}`)}>
                <span class="fleet-item-pri" data-pri={j.priority ?? 'standard'}>{j.priority ?? 'standard'}</span>
                <span class="fleet-item-title">{j.title}</span>
                <span class="fleet-item-age">{j.opened_on ?? ''}</span>
              </button>
            </li>
          {/each}
        </ul>
      {/if}
    </section>
  {/if}

  {@const tableRows = (() => {
    const live = new Set((fleet?.nodes ?? []).map((n) => n.slug));
    const flowOnly = (stageStats ?? [])
      .filter((s) => !live.has(s.slug) && s.completed > 0)
      .map((s) => ({ slug: s.slug, ready: 0, active: 0, unassigned: 0, by_role: {}, oldest_ready_wall: null }));
    return [...(fleet?.nodes ?? []), ...flowOnly];
  })()}
  {#if tableRows.length > 0}
    <table class="fleet-table">
      <thead>
        <tr>
          <th>Step</th>
          <th>Ready</th>
          <th>Active</th>
          <th>Unclaimed</th>
          <th>Role lenses</th>
          <th>Oldest wait</th>
          <th>Done (7d)</th>
          <th>p50 / max</th>
          <th>Expected wait</th>
        </tr>
      </thead>
      <tbody>
        {#each tableRows as n (n.slug)}
          {@const st = statBySlug.get(n.slug)}
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
            {#if stageStats === null}
              <!-- Unread, never a zero: "0 done" reads as "nothing drains" (ffc3444f). -->
              <td class="fleet-unread">unread</td>
              <td class="fleet-unread">unread</td>
              <td class="fleet-unread">unread</td>
            {:else}
              <td>{st?.completed ?? 0}</td>
              <td>{st && st.completed > 0 ? `${fmtDur(st.p50_seconds)} / ${fmtDur(st.max_seconds)}` : '—'}</td>
              <td title="Little's law: depth ÷ drain rate over the window — what a new arrival should expect if nothing changes">
                {st && st.completed > 0 && n.ready + n.active > 0
                  ? `~${fmtDur(((n.ready + n.active) * 7 * 86400) / st.completed)}`
                  : '—'}
              </td>
            {/if}
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
    color: var(--static);
  }
  .fleet-scope {
    font-size: 12px;
    color: var(--static);
  }
  .fleet-msg {
    margin: 24px 0;
    color: var(--static);
  }
  .fleet-note {
    margin: 8px 0;
    font-size: 13px;
  }
  .fleet-stale,
  .fleet-note.load-failed {
    color: var(--warn);
  }
  .fleet-unread {
    color: var(--static);
    font-style: italic;
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
    border-bottom: 1px solid var(--hairline);
  }
  .fleet-table th {
    font-weight: 600;
    color: var(--static);
  }
  .fleet-node-items {
    margin-top: 14px;
  }
  .fleet-node-h {
    font-size: 14px;
    display: flex;
    align-items: baseline;
    gap: 10px;
  }
  .fleet-node-n {
    font-size: 12px;
    font-weight: 400;
    color: var(--static);
  }
  .fleet-items {
    list-style: none;
    margin: 8px 0 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 6px;
    max-width: 720px;
  }
  .fleet-item {
    display: flex;
    align-items: baseline;
    gap: 10px;
    width: 100%;
    text-align: left;
    padding: 7px 12px;
    border: 1px solid var(--hairline);
    border-radius: 6px;
    background: var(--card);
    cursor: pointer;
    font: inherit;
    color: inherit;
  }
  .fleet-item-pri {
    font-size: 11px;
    font-weight: 600;
    text-transform: uppercase;
    color: var(--static);
  }
  .fleet-item-pri[data-pri='urgent'],
  .fleet-item-pri[data-pri='emergency'] {
    color: var(--err);
  }
  .fleet-item-title {
    flex: 1;
    font-size: 13px;
  }
  .fleet-item-age {
    font-size: 12px;
    color: var(--static);
  }
  .fleet-offmap {
    margin-left: 6px;
    font-size: 11px;
    font-weight: 600;
    color: var(--signal);
  }
</style>
