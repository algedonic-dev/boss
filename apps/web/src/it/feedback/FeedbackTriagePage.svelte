<script lang="ts">
  // /system/feedback — the triage board for user feedback.
  //
  // Feedback is a Job, so this is a read surface over Jobs of kind
  // `user-feedback` plus two writes that go through the ordinary
  // step API. There is no feedback table, no feedback endpoint, and
  // nothing here that another Job kind could not reuse.
  //
  // Columns are STEP STATE, not a status field someone maintains. A
  // Step's `status` is the program counter of the state machine, so
  // the board is a rendering of where each Job actually is rather
  // than a parallel bookkeeping of where someone said it was. That is
  // why moving a card is completing a step: the card cannot disagree
  // with the Job.
  //
  // The agent hand-off records a durable request on the step rather
  // than firing something. Two reasons. It survives a reload and is
  // visible to anyone else looking at the board, and — the one that
  // shaped it — an agent taking an automatic first pass later writes
  // the SAME record without a human clicking. If the button had just
  // called something, the automated path would have had nowhere to
  // leave its trace.
  import { onMount } from 'svelte';
  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import { session } from '@boss/web-kit/session/session.svelte';
  import { navigate } from '../../router';

  type Step = {
    id: string;
    kind: string;
    status: string;
    assignee_id?: string | null;
    metadata?: Record<string, unknown> | null;
  };
  type FeedbackJob = {
    id: string;
    title: string;
    status: string;
    subject_id?: string | null;
    owner_id?: string | null;
    opened_on?: string | null;
    metadata?: Record<string, unknown> | null;
    steps?: Step[];
  };

  let jobs = $state<ReadonlyArray<FeedbackJob>>([]);
  let loading = $state(true);
  let error = $state<string | null>(null);
  let busy = $state<Record<string, boolean>>({});

  let me = $derived(session.value.kind === 'ready' ? session.value.user.id : '');

  /// The triage step is where a feedback Job lives until someone acts.
  function triageStep(j: FeedbackJob): Step | undefined {
    return j.steps?.find((s) => s.kind === 'acknowledgment');
  }

  /// Which column a Job belongs in — derived from the step, never
  /// stored. `waiting` is the one an operator has to act on.
  type Column = 'waiting' | 'with-agent' | 'done';
  function columnOf(j: FeedbackJob): Column {
    const t = triageStep(j);
    if (!t || t.status === 'completed' || t.status === 'skipped') return 'done';
    return agentRequestedAt(j) ? 'with-agent' : 'waiting';
  }

  function agentRequestedAt(j: FeedbackJob): string | null {
    const md = triageStep(j)?.metadata as Record<string, unknown> | undefined;
    const v = md?.['agent_requested_at'];
    return typeof v === 'string' ? v : null;
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
    const out: Record<Column, FeedbackJob[]> = {
      waiting: [],
      'with-agent': [],
      done: [],
    };
    for (const j of jobs) out[columnOf(j)].push(j);
    return out;
  });

  async function load(): Promise<void> {
    loading = true;
    error = null;
    try {
      // The list endpoint enriches each Job with its steps, so the
      // board needs one request rather than one per card.
      const r = await fetch('/api/jobs?kind=user-feedback&limit=200');
      if (!r.ok) throw new Error(`feedback jobs: HTTP ${r.status}`);
      const body = await r.json();
      const rows: FeedbackJob[] = Array.isArray(body) ? body : (body.data ?? []);
      jobs = rows;
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      loading = false;
    }
  }

  /// PUT semantics on a step are read-overlay-write, and top-level
  /// metadata is replaced wholesale — so merge with what is already
  /// there or the other keys are wiped.
  async function patchStep(
    j: FeedbackJob,
    patch: Record<string, unknown>,
    metadata?: Record<string, unknown>,
  ): Promise<void> {
    const step = triageStep(j);
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

  const markTriaged = (j: FeedbackJob) => patchStep(j, { status: 'completed' });

  /// Record that an agent should take this one. Deliberately NOT a
  /// call to anything: the record is the point, and an automatic
  /// first pass writes the same shape later.
  const handToAgent = (j: FeedbackJob) =>
    patchStep(
      j,
      {},
      {
        agent_requested_at: new Date().toISOString(),
        agent_requested_by: me || 'anonymous',
      },
    );

  const recall = (j: FeedbackJob) =>
    patchStep(j, {}, { agent_requested_at: null, agent_requested_by: null });

  function message(j: FeedbackJob): string {
    const m = (j.metadata as Record<string, unknown> | undefined)?.['message'];
    return typeof m === 'string' ? m : '(no message)';
  }
  function route(j: FeedbackJob): string | null {
    const r = (j.metadata as Record<string, unknown> | undefined)?.['route'];
    return typeof r === 'string' ? r : (j.subject_id ?? null);
  }

  onMount(load);
</script>

<PageHeader
  title="Feedback triage"
  subtitle="Every item is a user-feedback Job. Columns are the triage step's state, so a card cannot disagree with the Job behind it."
/>

{#if loading}
  <p class="fb-msg">Loading feedback…</p>
{:else if error}
  <p class="fb-msg fb-err">{error}</p>
{:else if jobs.length === 0}
  <p class="fb-msg">
    No feedback yet. It arrives from the Feedback control in the top bar, on
    whichever page the person was looking at.
  </p>
{:else}
  <div class="fb-board">
    {#each COLUMNS as col (col.id)}
      {@const cards = byColumn[col.id]}
      <section class="fb-col" aria-label={col.label}>
        <header class="fb-col-head">
          <h3>{col.label}</h3>
          <span class="fb-count">{cards.length}</span>
        </header>
        <p class="fb-col-hint">{col.hint}</p>

        {#each cards as j (j.id)}
          <article class="fb-card">
            <p class="fb-card-msg">{message(j)}</p>
            <div class="fb-card-meta">
              {#if route(j)}
                <button
                  class="fb-route"
                  type="button"
                  onclick={() => navigate(route(j) ?? '/')}
                  title="Open the page this is about"
                >
                  {route(j)}
                </button>
              {/if}
              <span class="fb-by">{j.owner_id ?? 'unknown'}</span>
            </div>

            {#if agentRequestedAt(j)}
              <p class="fb-agent">
                Handed to an agent
                {#if (triageStep(j)?.metadata as Record<string, unknown> | undefined)?.['agent_requested_by']}
                  by {(triageStep(j)?.metadata as Record<string, unknown>)['agent_requested_by']}
                {/if}
              </p>
            {/if}

            {#if col.id !== 'done'}
              <div class="fb-actions">
                {#if agentRequestedAt(j)}
                  <button
                    class="fb-btn"
                    type="button"
                    disabled={busy[j.id]}
                    onclick={() => recall(j)}>Take back</button
                  >
                {:else}
                  <button
                    class="fb-btn"
                    type="button"
                    disabled={busy[j.id]}
                    onclick={() => handToAgent(j)}>Hand to agent</button
                  >
                {/if}
                <button
                  class="fb-btn fb-btn-primary"
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
  .fb-board {
    display: grid;
    grid-template-columns: repeat(3, minmax(0, 1fr));
    gap: 16px;
    align-items: start;
  }
  @media (max-width: 900px) {
    .fb-board {
      grid-template-columns: minmax(0, 1fr);
    }
  }
  .fb-col {
    display: flex;
    flex-direction: column;
    gap: 8px;
    background: var(--bg, #f5f5f4);
    border: 1px solid var(--border, #e7e5e4);
    border-radius: 8px;
    padding: 12px;
  }
  .fb-col-head {
    display: flex;
    align-items: baseline;
    gap: 8px;
  }
  .fb-col-head h3 {
    font-size: 13px;
    margin: 0;
  }
  .fb-count {
    font-size: 11px;
    color: var(--text-dim, #78716c);
    font-variant-numeric: tabular-nums;
  }
  .fb-col-hint {
    font-size: 11px;
    color: var(--text-dim, #a8a29e);
    margin: 0 0 4px;
  }
  .fb-card {
    background: var(--card, #fff);
    border: 1px solid var(--border, #e7e5e4);
    border-radius: 6px;
    padding: 10px;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .fb-card-msg {
    margin: 0;
    font-size: 13px;
    line-height: 1.45;
  }
  .fb-card-meta {
    display: flex;
    align-items: center;
    gap: 8px;
    flex-wrap: wrap;
    font-size: 11px;
    color: var(--text-dim, #78716c);
  }
  .fb-route {
    font: inherit;
    font-size: 11px;
    background: var(--bg, #f5f5f4);
    border: 1px solid var(--border, #e7e5e4);
    border-radius: 3px;
    padding: 1px 6px;
    cursor: pointer;
    color: inherit;
  }
  .fb-by {
    margin-left: auto;
  }
  .fb-agent {
    margin: 0;
    font-size: 11px;
    color: #b45309;
  }
  .fb-actions {
    display: flex;
    gap: 6px;
  }
  .fb-btn {
    font: inherit;
    font-size: 12px;
    padding: 3px 8px;
    border-radius: 4px;
    border: 1px solid var(--border, #e7e5e4);
    background: var(--bg, #f5f5f4);
    color: inherit;
    cursor: pointer;
  }
  .fb-btn-primary {
    background: #1c1917;
    color: #fff;
    border-color: #1c1917;
  }
  .fb-btn:disabled {
    opacity: 0.5;
    cursor: default;
  }
  .fb-msg {
    color: var(--text-dim, #78716c);
    font-size: 14px;
  }
  .fb-err {
    color: #b91c1c;
  }
</style>
