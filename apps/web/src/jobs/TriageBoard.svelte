<script lang="ts">
  // A queue of Jobs parked on a human, as a board.
  //
  // This started life as the feedback triage page and had nothing
  // feedback-shaped in it except the kind it filtered on. What it
  // actually renders is the general case: work that has reached a
  // step gated on somebody's authority and is waiting for a person to
  // act. That shape recurs — approvals, sign-offs, exception queues —
  // so the board is the component and the queue is a prop.
  //
  // Columns are STEP STATE, never a stored field. A Step's `status` is
  // the program counter of the state machine, so the board renders
  // where each Job actually is rather than a parallel bookkeeping of
  // where someone said it was. That is why moving a card is completing
  // a step: the card cannot disagree with the Job behind it.
  //
  // The gated step is found by its `authority_role` — the property
  // that makes it wait for a person — not by matching a step kind.
  // Kinds are registry data and a kind is a bundle of properties, so
  // matching one would pin today's spelling of a spec the registry is
  // free to re-author. Finding it by the authority gate also states
  // the real dependency: this board exists to show work parked on an
  // authorised human.
  //
  // The agent hand-off records a durable request rather than firing
  // something. It survives a reload, it is visible to anyone else
  // looking, and — the reason it is shaped this way — an agent taking
  // an automatic first pass later writes the SAME record with no human
  // clicking. A button that just called something would have left the
  // automated path nowhere to put its trace.
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
    /// carry a better headline (feedback puts it in `metadata.message`)
    /// supply their own. Everything else about a card — the owner, the
    /// agent badge, the actions — is generic and stays here.
    card?: Snippet<[Job]>;
  }>;

  let {
    kind,
    title,
    subtitle = 'Columns are the gated step’s state, so a card cannot disagree with the Job behind it.',
    emptyMessage = 'Nothing is waiting on a person right now.',
    card,
  }: Props = $props();

  let jobs = $state<ReadonlyArray<Job>>([]);
  let loading = $state(true);
  let error = $state<string | null>(null);
  let busy = $state<Record<string, boolean>>({});

  let me = $derived(session.value.kind === 'ready' ? session.value.user.id : '');

  /// The step this Job is parked on — the one gated on human authority.
  function gatedStep(j: Job): Step | undefined {
    return j.steps?.find(
      (s) => (s.metadata as Record<string, unknown> | undefined)?.['authority_role'],
    );
  }

  function agentRequestedAt(j: Job): string | null {
    const v = gatedStep(j)?.metadata?.['agent_requested_at'];
    return typeof v === 'string' ? v : null;
  }

  function agentRequestedBy(j: Job): string | null {
    const v = gatedStep(j)?.metadata?.['agent_requested_by'];
    return typeof v === 'string' ? v : null;
  }

  type Column = 'waiting' | 'with-agent' | 'done';
  function columnOf(j: Job): Column {
    const s = gatedStep(j);
    if (!s || s.status === 'completed' || s.status === 'skipped') return 'done';
    return agentRequestedAt(j) ? 'with-agent' : 'waiting';
  }

  const COLUMNS: ReadonlyArray<{ id: Column; label: string; hint: string }> = [
    { id: 'waiting', label: 'Waiting on triage', hint: 'Nobody has looked at these yet.' },
    {
      id: 'with-agent',
      label: 'With an agent',
      hint: 'Handed off for a first pass. Still open — an agent looking is not a decision.',
    },
    { id: 'done', label: 'Triaged', hint: 'Closed out.' },
  ];

  let byColumn = $derived.by(() => {
    const out: Record<Column, Job[]> = { waiting: [], 'with-agent': [], done: [] };
    for (const j of jobs) out[columnOf(j)].push(j);
    return out;
  });

  async function load(): Promise<void> {
    loading = true;
    error = null;
    try {
      // The list endpoint enriches each Job with its steps, so the
      // board needs one request rather than one per card.
      const r = await fetch(`/api/jobs?kind=${encodeURIComponent(kind)}&limit=200`);
      if (!r.ok) throw new Error(`${kind} jobs: HTTP ${r.status}`);
      const body = await r.json();
      jobs = Array.isArray(body) ? body : (body.data ?? []);
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
  /// this same metadata and is how `gatedStep` finds the step at all.
  /// A write that replaced metadata instead of merging would make the
  /// card vanish from the board on its first hand-off.
  async function patchStep(
    j: Job,
    patch: Record<string, unknown>,
    metadata?: Record<string, unknown>,
  ): Promise<void> {
    const step = gatedStep(j);
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

  const markTriaged = (j: Job) => patchStep(j, { status: 'completed' });

  /// Record that an agent should take this one. Deliberately NOT a
  /// call to anything: the record is the point.
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
  // Dragging a card IS the action the buttons perform — there is no
  // separate "move" concept, because a column is not a place a card
  // can be put. It is a rendering of the gated step's state, so the
  // only way to change which column a card is in is to change the
  // step. Every drop below routes to the same handlers the buttons
  // call.
  //
  // The buttons stay. Drag is an accelerator for the mouse, and it is
  // unusable by keyboard and awkward with a screen reader; removing
  // the buttons would make the board operable only by pointer. This
  // way the accessible path is the primary one and drag is additive.

  let dragging = $state<string | null>(null);
  let dragOver = $state<Column | null>(null);

  /// `done` is terminal — a completed step does not un-complete, so
  /// those cards do not lift and no drop targets them.
  const canDrag = (j: Job): boolean => columnOf(j) !== 'done';

  function startDrag(e: DragEvent, j: Job): void {
    if (!canDrag(j)) return;
    dragging = j.id;
    // Firefox refuses to start a drag without payload.
    e.dataTransfer?.setData('text/plain', j.id);
    if (e.dataTransfer) e.dataTransfer.effectAllowed = 'move';
  }

  function endDrag(): void {
    dragging = null;
    dragOver = null;
  }

  function onDragOver(e: DragEvent, col: Column): void {
    if (!dragging) return;
    // Preventing default is what marks this a valid drop target.
    e.preventDefault();
    if (e.dataTransfer) e.dataTransfer.dropEffect = 'move';
    dragOver = col;
  }

  function onDrop(e: DragEvent, col: Column): void {
    e.preventDefault();
    const id = dragging ?? e.dataTransfer?.getData('text/plain');
    endDrag();
    const j = jobs.find((x) => x.id === id);
    if (j) void moveTo(j, col);
  }

  /// Translate a drop into the step transition it means. Anything
  /// that is not a real transition is a no-op rather than an error —
  /// dropping a card back where it started is the common case.
  function moveTo(j: Job, target: Column): Promise<void> | void {
    const from = columnOf(j);
    if (from === target || from === 'done') return;
    if (target === 'done') return markTriaged(j);
    if (target === 'with-agent') return handToAgent(j);
    return recall(j);
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
    {#each COLUMNS as col (col.id)}
      {@const cards = byColumn[col.id]}
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

            {#if agentRequestedAt(j)}
              <p class="tb-agent">
                Handed to an agent{#if agentRequestedBy(j)} by {agentRequestedBy(j)}{/if}
              </p>
            {/if}

            {#if col.id !== 'done'}
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
                <button
                  class="tb-btn tb-btn-primary"
                  type="button"
                  disabled={busy[j.id]}
                  onclick={() => markTriaged(j)}>Mark triaged</button
                >
              </div>
            {/if}
          </article>
        {/each}
      </section>
    {/each}
  </div>
{/if}

<style>
  .tb-board {
    display: grid;
    grid-template-columns: repeat(3, minmax(0, 1fr));
    gap: 16px;
    align-items: start;
  }
  @media (max-width: 900px) {
    .tb-board {
      grid-template-columns: minmax(0, 1fr);
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
  .tb-col-over {
    border-color: #78716c;
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
  /* Dragging is a pointer accelerator; the buttons remain the
     operable path. Anyone who has asked not to see motion still gets
     the drag, they just do not get the fade. */
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
  .tb-msg {
    color: var(--text-dim, #78716c);
    font-size: 14px;
  }
  .tb-err {
    color: #b91c1c;
  }
</style>
