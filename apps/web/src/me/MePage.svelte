<script lang="ts">
  // Phase-0 port of apps/web/src/me/MePage.tsx.
  //
  // Scope-reduced for the spike: hero + "My Jobs" list + at-a-glance
  // count. Sub-panels (bulletins, next-actions, messages, certs) are
  // deferred to phase 1. The point is to exercise the hot paths —
  // state, $effect, $derived, fetch — not to match feature parity.
  //
  // Conceptual mapping (see docs/design/human-powered-state-machine.md):
  //   $state    → a cell of machine memory
  //   $derived  → a projection of that memory
  //   $effect   → a transition that reads/writes the world
  // This page uses all three.

  import { session } from '@boss/web-kit/session/session.svelte';
  import { appNow } from '@boss/web-kit/sim-clock';
  import {
    fetchMyDay,
    claimStep,
    assignmentPacket,
    type MyDayQueues,
    type AssignmentRow,
  } from './assignments';
  import {
    fetchWatchlist,
    windowNote,
    type WatchlistState,
  } from './watchlist';
  import PacketCard from '@boss/web-kit/ui/PacketCard.svelte';
  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import Section from '@boss/web-kit/ui/Section.svelte';

  // The session rune exposes the current user. The fetch effect
  // tracks exactly (id, role) — the two halves of the lens query:
  // the personal queue and the role's group queue.
  let userId = $derived(
    session.value.kind === 'ready' ? session.value.user.id : null,
  );
  let userRole = $derived(
    session.value.kind === 'ready' ? session.value.user.role : null,
  );

  // My Day is the assignments lens now (queue-visibility Q1): one
  // indexed call whose WHERE clause IS the queue definition, instead
  // of the capped jobs?status=open scan filtered client-side. The
  // page just presents the queues.
  let queues = $state<MyDayQueues | null>(null);
  let loading = $state(true);
  let claimNote = $state<string | null>(null);

  $effect(() => {
    const uid = userId;
    const role = userRole;
    if (!uid || !role) return;
    loading = true;
    let cancelled = false;
    (async () => {
      const q = await fetchMyDay(uid, role);
      if (!cancelled) {
        queues = q;
        loading = false;
      }
    })();
    return () => {
      cancelled = true;
    };
  });

  // The watchlist: packets THIS person filed, and what became of them.
  // A second station read, deliberately not folded into fetchMyDay —
  // the two answer different questions (work assigned to me vs. work I
  // reported) and one failing must not blank the other.
  //
  // Keyed on the user id only. The server binds `@me` from the session
  // itself, so this call carries no identity of its own; the id is here
  // to refetch when the signed-in person changes.
  let watchlist = $state<WatchlistState>({ kind: 'loading' });

  $effect(() => {
    if (!userId) return;
    let cancelled = false;
    (async () => {
      const w = await fetchWatchlist();
      if (!cancelled) watchlist = w;
    })();
    return () => {
      cancelled = true;
    };
  });

  // The claim hop. A 409 names the winner — losing a race is
  // ordinary queue life, so it reads as information, not an error.
  async function onClaim(row: AssignmentRow) {
    claimNote = null;
    const res = await claimStep(row.job_id, row.step.id);
    if (res.kind === 'conflict') {
      claimNote = `"${row.step.title}" was taken by ${res.holder ?? 'someone else'} first`;
    } else if (res.kind === 'error') {
      claimNote = `Claim failed (${res.message})`;
    }
    if (userId && userRole) {
      queues = await fetchMyDay(userId, userRole);
    }
  }

  function timeOfDay(): string {
    const h = new Date().getHours();
    if (h < 12) return 'morning';
    if (h < 17) return 'afternoon';
    return 'evening';
  }

  function tenureYears(hireDate: string): number {
    return (
      (appNow().getTime() - new Date(hireDate).getTime()) /
      (1000 * 60 * 60 * 24 * 365)
    );
  }
</script>

{#if session.value.kind === 'loading'}
  <div class="theme-exec" style="padding: 32px">Loading session…</div>
{:else if session.value.kind === 'unauthenticated'}
  <div class="theme-exec" style="padding: 32px">
    <p class="empty">
      Not signed in. Reload the page to log in.
    </p>
  </div>
{:else if session.value.kind === 'unrecognized'}
  <div class="theme-exec" style="padding: 32px">
    <p class="empty">
      Signed in as <strong>{session.value.username}</strong>, but no
      matching employee in the roster.
    </p>
  </div>
{:else}
  {@const user = session.value.user}
  <div class="theme-exec" style="padding: 0 32px 32px">
    <PageHeader
      eyebrow={`Good ${timeOfDay()}`}
      title={user.name}
      subtitle={`${user.role} · ${tenureYears(user.hire_date).toFixed(1)} years · ${user.department}`}
      motif="glass"
    />

    <div class="me-grid">
      <Section title="My queue" wide>
        {#if loading}
          <div class="myday-loading">Loading your queue…</div>
        {:else if !queues || queues.mine.length === 0}
          <div class="myday-empty">
            Nothing in your personal queue right now.
          </div>
        {:else}
          <!-- The same packet card the train yard deals — one card
               grammar across the network (d69033dd). Double-click or
               Enter opens the job detail. -->
          <div class="myday-jobs-list">
            {#each queues.mine as row (row.step.id)}
              <PacketCard card={assignmentPacket(row)} />
            {/each}
          </div>
        {/if}
      </Section>

      <Section title="Up for grabs" wide>
        {#if claimNote}
          <div class="myday-claim-note" role="status">{claimNote}</div>
        {/if}
        {#if !queues || queues.upForGrabs.length === 0}
          <div class="myday-empty">
            Nothing waiting on your role's queue.
          </div>
        {:else}
          <div class="myday-jobs-list">
            {#each queues.upForGrabs as row (row.step.id)}
              <!-- The claim hop stays a button beside the card: the
                   card is the packet, the claim is queue mechanics. -->
              <div class="myday-grab-row">
                <PacketCard card={assignmentPacket(row)} />
                <button class="myday-claim-btn" onclick={() => onClaim(row)}>
                  Claim
                </button>
              </div>
            {/each}
          </div>
          {#if queues.inFlightElsewhere.length > 0}
            <div class="myday-inflight-note">
              {queues.inFlightElsewhere.length} role-matched step{queues.inFlightElsewhere.length === 1 ? '' : 's'} in flight with teammates
            </div>
          {/if}
        {/if}
      </Section>

      <!-- Beneath the two work queues, because it is not work: it is
           the receipt. Read-only by construction — the packets here
           belong to whoever is handling them, and the only affordance
           is the card's own double-click to open the job. -->
      <Section title="My watchlist" wide>
        {#if watchlist.kind === 'loading'}
          <div class="myday-loading">Loading your watchlist…</div>
        {:else if watchlist.kind === 'unavailable'}
          <div class="myday-empty">
            The watchlist station hasn't reached this deployment yet.
          </div>
        {:else if watchlist.kind === 'error'}
          <div class="myday-empty">Couldn't load your watchlist.</div>
        {:else if watchlist.entries.length === 0}
          <div class="myday-empty">
            You haven't filed anything. Feedback you send from the
            Feedback button appears here, and stays until its outcome
            has been visible for a while.
          </div>
        {:else}
          {@const note = windowNote(watchlist.windowDays)}
          {#if note}
            <div class="watch-window">{note}</div>
          {/if}
          <div class="myday-jobs-list">
            {#each watchlist.entries as entry (entry.card.id)}
              <div class="watch-row">
                <PacketCard card={entry.card} />
                {#if entry.outcome}
                  <!-- The terminal state IS the information this
                       section exists for, so it sits beside the card
                       rather than blending into the card's tag chips
                       (all --static by design). -->
                  <span class="watch-outcome watch-{entry.outcome.tone}">
                    {entry.outcome.label}
                  </span>
                {/if}
              </div>
            {/each}
          </div>
        {/if}
      </Section>

      <Section title="At a glance">
        <div class="me-stats">
          <div class="me-stat-card">
            <div class="me-stat-num">{queues ? queues.mine.length : 0}</div>
            <div class="me-stat-label">steps in your queue</div>
          </div>
        </div>
      </Section>
    </div>
  </div>
{/if}

<style>
  /* Packet card + claim button side by side; the card takes the row. */
  .myday-grab-row {
    display: grid;
    grid-template-columns: 1fr auto;
    gap: 8px;
    align-items: center;
  }

  .myday-claim-btn {
    margin-left: auto;
    font-size: 11px;
    font-weight: 600;
    padding: 2px 10px;
    border: 1px solid var(--accent, #0284c7);
    color: var(--accent, #0284c7);
    background: transparent;
    border-radius: 4px;
    cursor: pointer;
  }
  .myday-claim-btn:hover {
    background: var(--accent, #0284c7);
    color: #fff;
  }
  .myday-claim-note {
    padding: 8px 12px;
    background: var(--bg, #f5f5f4);
    border: 1px solid var(--border, #d6d3d1);
    border-radius: 6px;
    font-size: 13px;
    margin: 0 0 12px 0;
  }
  .myday-inflight-note {
    margin-top: 8px;
    font-size: 12px;
    color: var(--text-dim, #78716c);
  }

  /* Card + outcome side by side, the same shape the claim row uses. */
  .watch-row {
    display: grid;
    grid-template-columns: 1fr auto;
    gap: 8px;
    align-items: center;
  }
  .watch-window {
    font-size: 12px;
    color: var(--static, #7a838c);
    margin: 0 0 8px 0;
  }
  /* Mono + caps, matching the packet card's own chip treatment; only
     the color changes, and only to a declared status token. */
  .watch-outcome {
    font-family: var(--font-mono, ui-monospace, monospace);
    font-size: 10px;
    letter-spacing: var(--ls-label, 0.1em);
    text-transform: uppercase;
    padding: 1px 6px;
    border: 1px solid currentColor;
    white-space: nowrap;
  }
  .watch-ok {
    color: var(--ok, #4fb98a);
  }
  .watch-warn {
    color: var(--warn, #d9a441);
  }
  .watch-static {
    color: var(--static, #7a838c);
  }
</style>
