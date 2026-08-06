<script lang="ts">
  // A queue of Jobs parked on a human, as a board.
  //
  // Columns come from the JobKind, not from this file. A triage step
  // that forks declares its dispositions as an inline enum field, and
  // each disposition has a successor step gated on it — so the board
  // renders one column per route the workflow actually offers, labelled
  // with that successor's own title. Add a disposition to the registry
  // and the column appears; nothing here changes.
  //
  // That is the whole point of the redesign. The first version had
  // three hardcoded columns ending in "Triaged", which made triage a
  // synonym for closing. Triage's real output is a decision about what
  // happens next, so dropping a card into a column IS choosing that
  // route: it completes the fork step with that disposition, which
  // makes the corresponding next step ready.
  //
  // Columns therefore remain STEP STATE, never a stored field. A
  // Step's `status` is the program counter of the state machine, so a
  // card cannot disagree with the Job behind it.
  //
  // The agent hand-off is an annotation on an untriaged card rather
  // than a column: an agent taking a first pass is not a disposition,
  // and treating it as one was the modelling error. It records a
  // durable request rather than firing something — it survives a
  // reload, and an agent taking an automatic first pass later writes
  // the same record with no human clicking.
  import type { Snippet } from 'svelte';
  import { onMount } from 'svelte';
  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import { session } from '@boss/web-kit/session/session.svelte';
  import type { Job, Step } from './types';

  type Props = Readonly<{
    /// Which queue this board shows. One JobKind today because that is
    /// what `JobFilter` can push into SQL; a board over "everything
    /// awaiting a human" needs a server-side filter that does not
    /// exist yet, and doing it client-side would silently truncate.
    kind: string;
    title: string;
    subtitle?: string;
    emptyMessage?: string;
    /// The card body. Defaults to the Job title; callers whose Jobs
    /// carry a better headline supply their own.
    card?: Snippet<[Job]>;
  }>;

  let {
    kind,
    title,
    subtitle = 'Routing an item completes its triage step, which opens the next one — so a card cannot disagree with the Job behind it.',
    emptyMessage = 'Nothing is waiting on a person right now.',
    card,
  }: Props = $props();

  type Option = Readonly<{ value: string; label: string }>;
  type Fork = Readonly<{ field: string; options: ReadonlyArray<Option> }>;

  let jobs = $state<ReadonlyArray<Job>>([]);
  let fork = $state<Fork | null>(null);
  let loading = $state(true);
  let error = $state<string | null>(null);
  let busy = $state<Record<string, boolean>>({});
  let choice = $state<Record<string, string>>({});

  let me = $derived(session.value.kind === 'ready' ? session.value.user.id : '');

  const WAITING = '__waiting__';

  /// The step a Job is parked on: the one gated on human authority.
  /// Found by that property rather than by matching a step kind —
  /// kinds are registry data and a kind is a bundle of properties, so
  /// matching one would pin today's spelling of a spec the registry is
  /// free to re-author.
  function gatedStep(j: Job): Step | undefined {
    return j.steps?.find(
      (s) => (s.metadata as Record<string, unknown> | undefined)?.['authority_role'],
    );
  }

  /// The fork step ON A JOB — the gated step that asks for a
  /// disposition. Identified by carrying the enum field, so it stays
  /// correct if the spec renames or reorders steps.
  function forkStep(j: Job): Step | undefined {
    const field = fork?.field;
    if (!field) return gatedStep(j);
    return j.steps?.find((s) => s.fields?.some((f) => f.name === field));
  }

  function isTerminal(s: Step | undefined): boolean {
    return s?.status === 'completed' || s?.status === 'skipped';
  }

  function agentRequestedAt(j: Job): string | null {
    const v = forkStep(j)?.metadata?.['agent_requested_at'];
    return typeof v === 'string' ? v : null;
  }

  function agentRequestedBy(j: Job): string | null {
    const v = forkStep(j)?.metadata?.['agent_requested_by'];
    return typeof v === 'string' ? v : null;
  }

  /// Which column a Job sits in — derived, never stored. Untriaged
  /// items wait; triaged ones sit under the route they were sent to.
  function columnOf(j: Job): string {
    const s = forkStep(j);
    if (!isTerminal(s)) return WAITING;
    const chosen = fork ? s?.metadata?.[fork.field] : undefined;
    return typeof chosen === 'string' ? chosen : 'done';
  }

  let columns = $derived.by(() => {
    const head = [{ id: WAITING, label: 'Waiting on triage', hint: 'Nobody has routed these yet.' }];
    if (!fork) {
      return [...head, { id: 'done', label: 'Done', hint: 'Closed out.' }];
    }
    return [
      ...head,
      ...fork.options.map((o) => ({
        id: o.value,
        label: o.label,
        hint: `Routed here at triage.`,
      })),
    ];
  });

  let byColumn = $derived.by(() => {
    const out: Record<string, Job[]> = {};
    for (const c of columns) out[c.id] = [];
    for (const j of jobs) {
      const col = columnOf(j);
      (out[col] ??= []).push(j);
    }
    return out;
  });

  /// Read the queue's fork out of the JobKind registry: the step with
  /// a required pipe-shaped field is the fork, its values are the
  /// dispositions, and each successor's `title_template` is that
  /// route's human name. Deriving the label from the successor rather
  /// than humanising the slug means the column says what the next step
  /// IS — "Reproduce and investigate", not "Reproduce".
  function readFork(spec: unknown): Fork | null {
    const steps = (spec as { steps?: unknown[] })?.steps;
    if (!Array.isArray(steps)) return null;

    for (const step of steps) {
      const fields = (step as { fields?: unknown[] }).fields ?? [];
      for (const f of fields) {
        const field = f as { name?: string; field_type?: string; required?: boolean };
        if (!field.required || !field.name || !field.field_type?.includes('|')) continue;
        const options = field.field_type.split('|').map((value) => {
          const successor = steps.find((s) =>
            (s as { ready_when?: string }).ready_when?.includes(`${field.name} = "${value}"`),
          ) as { title_template?: string; title?: string } | undefined;
          return {
            value,
            label: successor?.title_template || successor?.title || value,
          };
        });
        return { field: field.name, options };
      }
    }
    return null;
  }

  async function load(): Promise<void> {
    loading = true;
    error = null;
    try {
      const [jobsRes, kindsRes] = await Promise.all([
        // The list endpoint enriches each Job with its steps, so the
        // board needs one request rather than one per card.
        fetch(`/api/jobs?kind=${encodeURIComponent(kind)}&limit=200`),
        fetch('/api/jobs/kinds'),
      ]);
      if (!jobsRes.ok) throw new Error(`${kind} jobs: HTTP ${jobsRes.status}`);
      const body = await jobsRes.json();
      jobs = Array.isArray(body) ? body : (body.data ?? []);

      // A missing registry costs the columns, not the board — the
      // cards still render and the fallback is waiting/done.
      if (kindsRes.ok) {
        const kinds = await kindsRes.json();
        const rows: unknown[] = Array.isArray(kinds) ? kinds : (kinds.data ?? []);
        fork = readFork(rows.find((k) => (k as { kind?: string }).kind === kind));
      }
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      loading = false;
    }
  }

  /// PUT semantics on a step are read-overlay-write, and top-level
  /// metadata is replaced wholesale — so merge with what is already
  /// there or the other keys are wiped.
  ///
  /// The merge is load-bearing, not hygiene: `authority_role` lives in
  /// this same metadata and is how the fork step is found at all. A
  /// write that replaced metadata would make the card vanish on its
  /// first hand-off.
  async function patchStep(
    j: Job,
    patch: Record<string, unknown>,
    metadata?: Record<string, unknown>,
  ): Promise<void> {
    const step = forkStep(j);
    if (!step || busy[j.id]) return;
    busy = { ...busy, [j.id]: true };
    try {
      const body: Record<string, unknown> = { ...patch };
      if (metadata) body.metadata = { ...(step.metadata ?? {}), ...metadata };
      const r = await fetch(`/api/jobs/${j.id}/steps/${step.id}`, {
        method: 'PUT',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(body),
      });
      if (!r.ok) throw new Error(`HTTP ${r.status}: ${await r.text()}`);
      await load();
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      busy = { ...busy, [j.id]: false };
    }
  }

  /// Routing IS triaging: completing the fork step with a disposition
  /// is what opens the next step. There is no separate "move".
  function route(j: Job, disposition: string): Promise<void> | void {
    if (!fork) return patchStep(j, { status: 'completed' });
    if (!fork.options.some((o) => o.value === disposition)) return;
    return patchStep(j, { status: 'completed' }, { [fork.field]: disposition });
  }

  const handToAgent = (j: Job) =>
    patchStep(
      j,
      {},
      { agent_requested_at: new Date().toISOString(), agent_requested_by: me || 'anonymous' },
    );

  const recall = (j: Job) =>
    patchStep(j, {}, { agent_requested_at: null, agent_requested_by: null });

  // ---- dragging ----------------------------------------------------
  //
  // Dragging a card is the same act as picking a route from the menu —
  // both complete the fork step with that disposition. The menu stays
  // because drag is unusable by keyboard and awkward with a screen
  // reader; removing it would make the board operable only by pointer.

  let dragging = $state<string | null>(null);
  let dragOver = $state<string | null>(null);

  /// Only untriaged cards lift. A completed fork step does not
  /// un-complete, so a routed card cannot be re-routed by dragging —
  /// offering a gesture that silently did nothing would be worse than
  /// offering none.
  const canDrag = (j: Job): boolean => columnOf(j) === WAITING;

  function startDrag(e: DragEvent, j: Job): void {
    if (!canDrag(j)) return;
    dragging = j.id;
    e.dataTransfer?.setData('text/plain', j.id);
    if (e.dataTransfer) e.dataTransfer.effectAllowed = 'move';
  }

  function endDrag(): void {
    dragging = null;
    dragOver = null;
  }

  /// Every column except `waiting` is a route a lifted card can take.
  const isDropTarget = (col: string): boolean => dragging !== null && col !== WAITING;

  function onDragOver(e: DragEvent, col: string): void {
    if (!isDropTarget(col)) return;
    e.preventDefault();
    if (e.dataTransfer) e.dataTransfer.dropEffect = 'move';
    dragOver = col;
  }

  function onDrop(e: DragEvent, col: string): void {
    e.preventDefault();
    const id = dragging ?? e.dataTransfer?.getData('text/plain');
    const wasTarget = isDropTarget(col);
    endDrag();
    if (!wasTarget) return;
    const j = jobs.find((x) => x.id === id);
    if (j) void route(j, col);
  }

  onMount(load);
</script>

<PageHeader {title} {subtitle} />

{#if loading}
  <p class="tb-msg">Loading…</p>
{:else if error}
  <p class="tb-msg tb-err">{error}</p>
{:else if jobs.length === 0}
  <p class="tb-msg">{emptyMessage}</p>
{:else}
  <div class="tb-board">
    {#each columns as col (col.id)}
      {@const cards = byColumn[col.id] ?? []}
      <!-- svelte-ignore a11y_no_static_element_interactions -->
      <section
        class="tb-col"
        class:tb-col-over={dragOver === col.id && dragging !== null}
        aria-label={col.label}
        ondragover={(e) => onDragOver(e, col.id)}
        ondragleave={() => (dragOver = null)}
        ondrop={(e) => onDrop(e, col.id)}
      >
        <header class="tb-col-head">
          <h3>{col.label}</h3>
          <span class="tb-count">{cards.length}</span>
        </header>
        <p class="tb-col-hint">{col.hint}</p>

        {#if isDropTarget(col.id)}
          <p class="tb-drop-zone" class:tb-drop-zone-over={dragOver === col.id}>
            Route here
          </p>
        {:else if cards.length === 0}
          <p class="tb-col-empty">Nothing here.</p>
        {/if}

        {#each cards as j (j.id)}
          <article
            class="tb-card"
            class:tb-card-draggable={canDrag(j)}
            class:tb-card-dragging={dragging === j.id}
            draggable={canDrag(j)}
            ondragstart={(e) => startDrag(e, j)}
            ondragend={endDrag}
          >
            {#if card}
              {@render card(j)}
            {:else}
              <p class="tb-card-title">{j.title}</p>
            {/if}

            <div class="tb-card-meta">
              <span class="tb-by">{j.owner_id || 'unassigned'}</span>
            </div>

            {#if col.id === WAITING}
              {#if agentRequestedAt(j)}
                <p class="tb-agent">
                  With an agent{#if agentRequestedBy(j)} — {agentRequestedBy(j)}{/if}
                </p>
              {/if}

              <div class="tb-actions">
                {#if fork}
                  <label class="tb-sr" for={`route-${j.id}`}>Route this item</label>
                  <select
                    id={`route-${j.id}`}
                    class="tb-select"
                    bind:value={choice[j.id]}
                    disabled={busy[j.id]}
                  >
                    <option value="" disabled selected>Route to…</option>
                    {#each fork.options as o (o.value)}
                      <option value={o.value}>{o.label}</option>
                    {/each}
                  </select>
                  <button
                    class="tb-btn tb-btn-primary"
                    type="button"
                    disabled={busy[j.id] || !choice[j.id]}
                    onclick={() => route(j, choice[j.id] ?? '')}>Route</button
                  >
                {:else}
                  <button
                    class="tb-btn tb-btn-primary"
                    type="button"
                    disabled={busy[j.id]}
                    onclick={() => route(j, 'done')}>Mark done</button
                  >
                {/if}
              </div>

              <div class="tb-actions">
                {#if agentRequestedAt(j)}
                  <button class="tb-btn" type="button" disabled={busy[j.id]} onclick={() => recall(j)}
                    >Take back</button
                  >
                {:else}
                  <button
                    class="tb-btn"
                    type="button"
                    disabled={busy[j.id]}
                    onclick={() => handToAgent(j)}>Hand to agent</button
                  >
                {/if}
              </div>
            {/if}
          </article>
        {/each}
      </section>
    {/each}
  </div>
{/if}

<style>
  /* Columns scroll sideways rather than squeezing: a fork can declare
     six routes, and a 140px column is unreadable. The page body never
     scrolls horizontally — this container does. */
  .tb-board {
    display: grid;
    grid-auto-flow: column;
    grid-auto-columns: minmax(230px, 1fr);
    gap: 16px;
    align-items: start;
    overflow-x: auto;
    padding-bottom: 8px;
  }
  @media (max-width: 900px) {
    .tb-board {
      grid-auto-flow: row;
      grid-auto-columns: auto;
    }
  }
  .tb-col {
    display: flex;
    flex-direction: column;
    gap: 8px;
    background: var(--bg, #f5f5f4);
    border: 1px solid var(--border, #e7e5e4);
    border-radius: 8px;
    padding: 12px;
    /* A column must be a target worth aiming at even with nothing in
       it, or the only droppable columns are the ones that already
       have cards — which is backwards. */
    min-height: 160px;
  }
  .tb-col-over {
    border-color: #78716c;
    background: var(--card, #fff);
  }
  .tb-col-head {
    display: flex;
    align-items: baseline;
    gap: 8px;
  }
  .tb-col-head h3 {
    font-size: 13px;
    margin: 0;
  }
  .tb-count {
    font-size: 11px;
    color: var(--text-dim, #78716c);
    font-variant-numeric: tabular-nums;
  }
  .tb-col-hint {
    font-size: 11px;
    color: var(--text-dim, #a8a29e);
    margin: 0 0 4px;
  }
  .tb-col-empty {
    margin: 0;
    font-size: 11px;
    color: var(--text-dim, #a8a29e);
    font-style: italic;
  }
  .tb-drop-zone {
    margin: 0;
    padding: 14px 10px;
    border: 1px dashed var(--text-dim, #a8a29e);
    border-radius: 6px;
    text-align: center;
    font-size: 12px;
    color: var(--text-dim, #78716c);
  }
  .tb-drop-zone-over {
    border-color: #1c1917;
    border-style: solid;
    color: #1c1917;
    background: var(--card, #fff);
  }
  .tb-card {
    background: var(--card, #fff);
    border: 1px solid var(--border, #e7e5e4);
    border-radius: 6px;
    padding: 10px;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .tb-card-draggable {
    cursor: grab;
  }
  .tb-card-draggable:active {
    cursor: grabbing;
  }
  .tb-card-dragging {
    opacity: 0.45;
  }
  @media (prefers-reduced-motion: reduce) {
    .tb-card-dragging {
      opacity: 1;
      outline: 2px dashed #78716c;
    }
  }
  .tb-card-title {
    margin: 0;
    font-size: 13px;
    line-height: 1.45;
  }
  .tb-card-meta {
    display: flex;
    align-items: center;
    gap: 8px;
    flex-wrap: wrap;
    font-size: 11px;
    color: var(--text-dim, #78716c);
  }
  .tb-by {
    margin-left: auto;
  }
  .tb-agent {
    margin: 0;
    font-size: 11px;
    color: #b45309;
  }
  .tb-actions {
    display: flex;
    gap: 6px;
    align-items: center;
  }
  .tb-select {
    font: inherit;
    font-size: 12px;
    padding: 3px 6px;
    border-radius: 4px;
    border: 1px solid var(--border, #e7e5e4);
    background: var(--card, #fff);
    color: inherit;
    flex: 1 1 auto;
    min-width: 0;
  }
  .tb-btn {
    font: inherit;
    font-size: 12px;
    padding: 3px 8px;
    border-radius: 4px;
    border: 1px solid var(--border, #e7e5e4);
    background: var(--bg, #f5f5f4);
    color: inherit;
    cursor: pointer;
  }
  .tb-btn-primary {
    background: #1c1917;
    color: #fff;
    border-color: #1c1917;
  }
  .tb-btn:disabled {
    opacity: 0.5;
    cursor: default;
  }
  .tb-sr {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip-path: inset(50%);
    white-space: nowrap;
  }
  .tb-msg {
    color: var(--text-dim, #78716c);
    font-size: 14px;
  }
  .tb-err {
    color: #b91c1c;
  }
</style>
