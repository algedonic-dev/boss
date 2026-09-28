<script lang="ts">
  // /it/experiments — the surface for controlled change.
  //
  // This replaces a placeholder that reserved the slot and fetched
  // nothing. What makes it worth building now is that the machinery
  // beneath it exists: `protocol-experiment` packets carry a
  // hypothesis stated BEFORE the result (the step order enforces it —
  // `measure` cannot open until `state` closes), and the per-version
  // terminal report already groups packets by their pinned
  // workflow_version.
  //
  // Two panels, because an experiment has two honest states:
  //
  //   * Running — the hypothesis and decision rule are visible while
  //     the answer is still unknown. That is the whole point: a
  //     prediction you can read before the number lands is falsifiable;
  //     one you read afterwards is a story.
  //   * Concluded — what was predicted, what was measured, what was
  //     decided, and the confounds admitted. Retired experiments are
  //     shown as prominently as promoted ones. A surface that quietly
  //     drops refuted hypotheses teaches people to stop writing them
  //     down.
  //
  // Failure renders as failure, per IncidentsPage: an experiments page
  // that shows an empty calm while the API is down is worse than one
  // that says it cannot read.
  import { onMount } from 'svelte';
  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import { href } from '../../router';
  import type { Job } from '../../jobs/types';
  import { standingOf } from '../../jobs/position';
  import { fetchEvery, wholeOrThrow } from '../../data/paginated';
  import { armOf, concludedCard, measuredLine, outcomeOf, stepField } from './experimentCard';

  type LoadState =
    | { kind: 'loading' }
    | { kind: 'failed'; message: string }
    | { kind: 'ready'; jobs: ReadonlyArray<Job> };

  let load = $state<LoadState>({ kind: 'loading' });

  async function fetchExperiments(): Promise<void> {
    load = { kind: 'loading' };
    try {
      // EVERY experiment, running and concluded: the archive is the
      // point of the page, and one page of 200 would have dropped the
      // oldest conclusions in silence once the kind passed 200
      // (backlog b68a9dde). A read that stops short fails, saying how
      // many of how many it held.
      const jobs = wholeOrThrow(
        await fetchEvery<Job>('/api/jobs?kind=protocol-experiment&full=true'),
      );
      load = { kind: 'ready', jobs };
    } catch (e) {
      load = { kind: 'failed', message: e instanceof Error ? e.message : String(e) };
    }
  }
  onMount(fetchExperiments);

  const jobs = $derived(load.kind === 'ready' ? load.jobs : []);

  const running = $derived(
    [...jobs.filter((j) => j.status !== 'closed' && j.status !== 'cancelled')].sort(
      (a, b) => (b.opened_on ?? '').localeCompare(a.opened_on ?? ''),
    ),
  );
  const concluded = $derived(
    [...jobs.filter((j) => j.status === 'closed' || j.status === 'cancelled')].sort(
      (a, b) => (b.closed_on ?? '').localeCompare(a.closed_on ?? ''),
    ),
  );

  // The readers live in experimentCard.ts, pinned there by its unit
  // test and here by experiments-page.mocked.spec.ts (page-audit
  // ec8351f4).

  /// The step someone can act on now — what the experiment is waiting
  /// for — with its status beside it, so a terminal like `promoted`
  /// standing ready does not read as already promoted (3102fe7a).
  const waitingOn = (j: Job): string =>
    standingOf({ ...j, steps: [...(j.steps ?? [])].sort((a, b) => a.sort_order - b.sort_order) }) ?? '';
</script>

<PageHeader
  title="Experiments"
  subtitle="Controlled change: a hypothesis on the record before the result."
/>

{#if load.kind === 'loading'}
  <p class="exp-msg">Loading experiments…</p>
{:else if load.kind === 'failed'}
  <div class="exp-failed" role="alert">
    <p class="exp-failed-text load-failed">
      Could not read experiments — {load.message}. This panel is blank because the
      record is unreachable, not because nothing is running.
    </p>
    <button class="exp-btn" type="button" onclick={fetchExperiments}>Retry</button>
  </div>
{:else}
  <section class="exp-running" aria-label="Running experiments">
    <h2 class="exp-h2">Running <span class="exp-count">{running.length}</span></h2>
    {#if running.length === 0}
      <!-- Zero is the decided state, not a lapse: experimentation starts
           at a named threshold (design d8771dec, David 2026-09-23), so
           the empty panel says when it will fill (backlog 071ffd8b). -->
      <p class="exp-empty">
        No experiment is running. One opens at the first draft workflow version, on a
        kind with 20+ terminals a day, that adds, removes or reorders a step. Its author
        opens it as a split instead of publishing.
      </p>
    {:else}
      {#each running as j (j.id)}
        <article class="exp-card">
          <header class="exp-card-head">
            <a class="exp-title" href={href(`/ux/jobs/${j.id}`)}>{j.title}</a>
            <span class="exp-waiting">waiting on: {waitingOn(j) || '—'}</span>
          </header>
          {#if stepField(j, 'state', 'hypothesis')}
            <dl class="exp-dl">
              <dt>Hypothesis</dt>
              <dd>{stepField(j, 'state', 'hypothesis')}</dd>
              <dt>Metric</dt>
              <dd>{stepField(j, 'state', 'metric')}</dd>
              <dt>Arms</dt>
              <dd>{armOf(j, 'control')} vs {armOf(j, 'candidate')}</dd>
              <dt>Decision rule</dt>
              <dd>{stepField(j, 'state', 'decision_rule')}</dd>
            </dl>
          {:else}
            <p class="exp-note">
              No hypothesis recorded yet — the experiment cannot measure until it states one.
            </p>
          {/if}
        </article>
      {/each}
    {/if}
  </section>

  <section class="exp-concluded" aria-label="Concluded experiments">
    <h2 class="exp-h2">Concluded <span class="exp-count">{concluded.length}</span></h2>
    {#if concluded.length === 0}
      <p class="exp-empty">Nothing concluded yet.</p>
    {:else}
      {#each concluded as j (j.id)}
        {@const c = concludedCard(j)}
        <article class="exp-card exp-card-done">
          <header class="exp-card-head">
            <a class="exp-title" href={href(`/ux/jobs/${j.id}`)}>{j.title}</a>
            <span class="exp-outcome exp-outcome-{outcomeOf(j) || 'none'}">{outcomeOf(j) || '—'}</span>
          </header>
          <dl class="exp-dl">
            <dt>Predicted</dt>
            <dd>{c.predicted || '—'}</dd>
            {#if c.kind === 'abandoned'}
              <!-- Abandoned closes after `state` with no measure or decide:
                   the terminal's reason is the record (backlog baf0c973). -->
              <dt>Abandoned</dt>
              <dd class="exp-abandoned-reason">{c.reason || '—'}</dd>
            {:else}
              <!-- Source and the floor stated in advance sit beside n; the
                   decision sits over its reading against the rule written
                   before the result (backlog 3633a918). -->
              <dt>Measured</dt>
              <dd class="exp-measured">{measuredLine(c.measured)}</dd>
              <dt>Decided</dt>
              <dd class="exp-decided">{c.decision || '—'}</dd>
              <dt>Against stated rule</dt>
              <dd class="exp-against-rule">{c.againstRule || '—'}</dd>
              {#if c.confounds}
                <dt>Confounds</dt>
                <dd class="exp-confounds">{c.confounds}</dd>
              {/if}
            {/if}
          </dl>
        </article>
      {/each}
    {/if}
  </section>
{/if}

<style>
  .exp-msg,
  .exp-empty,
  .exp-note {
    color: var(--static);
    font-size: 0.9rem;
  }
  .exp-failed {
    border: 1px solid var(--err);
    border-radius: var(--radius);
    padding: 0.75rem 1rem;
    margin-bottom: 1rem;
  }
  .exp-failed-text {
    margin: 0 0 0.5rem;
  }
  .exp-h2 {
    font-size: 1rem;
    margin: 1.25rem 0 0.5rem;
  }
  .exp-count {
    color: var(--static);
    font-weight: 400;
  }
  .exp-card {
    border: 1px solid var(--border);
    border-radius: var(--radius);
    padding: 0.75rem 1rem;
    margin-bottom: 0.75rem;
  }
  .exp-card-head {
    display: flex;
    justify-content: space-between;
    align-items: baseline;
    gap: 1rem;
  }
  .exp-title {
    font-weight: 600;
  }
  .exp-waiting,
  .exp-outcome {
    font-size: 0.8rem;
    color: var(--static);
    white-space: nowrap;
  }
  .exp-dl {
    display: grid;
    grid-template-columns: max-content 1fr;
    gap: 0.25rem 0.75rem;
    margin: 0.5rem 0 0;
    font-size: 0.875rem;
  }
  .exp-dl dt {
    color: var(--static);
  }
  .exp-dl dd {
    margin: 0;
  }
  .exp-confounds {
    color: var(--static);
  }
</style>
