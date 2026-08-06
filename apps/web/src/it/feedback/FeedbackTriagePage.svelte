<script lang="ts">
  // /system/feedback — the feedback queue.
  //
  // Everything structural lives in `TriageBoard`: the columns, the
  // agent hand-off, completing the gated step. This file is what is
  // actually specific to feedback — which queue to show, what to call
  // it, and the fact that a feedback Job's headline is its message
  // rather than its title ("Feedback on /ux/jobs" tells a triager
  // nothing they cannot already see from the route chip).
  //
  // If that ratio looks lopsided, that is the point. Adding the next
  // triage queue should be a route and a filter, not another board.
  import TriageBoard from '../../jobs/TriageBoard.svelte';
  import type { Job } from '../../jobs/types';
  import { navigate } from '../../router';

  function message(j: Job): string {
    const m = j.metadata?.['message'];
    return typeof m === 'string' ? m : '(no message)';
  }

  /// The surface the feedback is about. Falls back to the Subject id,
  /// which is the same value — the route path IS the Subject id.
  function route(j: Job): string | null {
    const r = j.metadata?.['route'];
    if (typeof r === 'string') return r;
    return j.subject?.id ?? null;
  }
</script>

<TriageBoard
  kind="user-feedback"
  title="Feedback triage"
  subtitle="Every item is a user-feedback Job. Columns are the triage step's state, so a card cannot disagree with the Job behind it."
  emptyMessage="No feedback yet. It arrives from the Feedback control in the top bar, on whichever page the person was looking at."
>
  {#snippet card(j)}
    <p class="fb-card-msg">{message(j)}</p>
    {#if route(j)}
      <div class="fb-card-route">
        <button
          class="fb-route"
          type="button"
          onclick={() => navigate(route(j) ?? '/')}
          title="Open the page this is about"
        >
          {route(j)}
        </button>
      </div>
    {/if}
  {/snippet}
</TriageBoard>

<style>
  .fb-card-msg {
    margin: 0;
    font-size: 13px;
    line-height: 1.45;
  }
  .fb-card-route {
    display: flex;
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
</style>
