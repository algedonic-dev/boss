<script lang="ts">
  // /it/registry/agents — the agents registry as a directory (backlog
  // 62988516). David, 2026-09-26: "Agents can go under a page in the IT
  // department" — never the People roster. What each field is, and why
  // there is no reports-to line, is agents.ts's header; this file only
  // draws it.
  //
  // READING ORDER: one card per registry row — who it is and what it
  // signs as, then the caps the registry declares, then what it holds
  // and what it has finished. The roster is one read; each card's runs
  // and held steps are that card's own reads, so one failing is said
  // on that card and does not blank the directory.
  import { onMount } from 'svelte';
  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import EntityLink from '@boss/web-kit/ui/EntityLink.svelte';
  import Link from '@boss/web-kit/ui/Link.svelte';
  import { href } from '../../router';
  import { regionHref } from '../yard/regions';
  import {
    budgetText,
    changeText,
    costText,
    effortsRun,
    fetchAgentDetail,
    fetchAgents,
    heldText,
    parseSeats,
    RUN_WINDOW,
    seatAgent,
    seatsText,
    type Agent,
    type AgentDetail,
    type SeatOutcome,
  } from './agents';
  import type { Remote } from '../../data/remote';

  let roster = $state<Remote<ReadonlyArray<Agent>>>({ kind: 'loading' });
  let details = $state<Readonly<Record<string, AgentDetail>>>({});
  // The seats typed per agent, and each card's last write (backlog
  // cdbebc22). What the write does and who may make it is agents.ts's
  // header; this page sends what was typed and draws what came back.
  let drafts = $state<Readonly<Record<string, string>>>({});
  let seating = $state<Readonly<Record<string, SeatOutcome | 'sending'>>>({});

  // The card's row becomes the registry's answer, so the seats it shows —
  // which the next write sends as `expected` — are never this page's memory.
  function show(row: Agent): void {
    if (roster.kind !== 'ready') return;
    roster = { kind: 'ready', data: roster.data.map((x) => (x.id === row.id ? row : x)) };
  }

  async function seat(a: Agent): Promise<void> {
    seating = { ...seating, [a.id]: 'sending' };
    // `expected` is the seats this card SHOWS: the door writes only while
    // the registry still holds them (review 1a49c3a7 finding 1).
    const out = await seatAgent(a.id, a.stations, parseSeats(drafts[a.id] ?? a.stations.join(', ')));
    seating = { ...seating, [a.id]: out };
    if (out.kind === 'applied') {
      show(out.after);
      drafts = { ...drafts, [a.id]: out.after.stations.join(', ') };
    } else if ((out.kind === 'stale' || out.kind === 'unknown') && out.held.kind === 'ready') {
      show(out.held.data);
    }
  }

  onMount(() => {
    void (async () => {
      roster = await fetchAgents();
      if (roster.kind !== 'ready') return;
      await Promise.all(
        roster.data.map(async (a) => {
          const d = await fetchAgentDetail(a);
          details = { ...details, [a.id]: d };
        }),
      );
    })();
  });

  const when = (iso: string | null): string => (iso ? `${iso.slice(0, 10)} ${iso.slice(11, 16)}Z` : '—');
</script>

<div class="ag-root">
  <PageHeader
    eyebrow="IT · Registry · who runs work besides people"
    title="Agents"
    subtitle="The agents registry: every agent that can hold a step, as the registry declares it. Agents are never People rows and never headcount. An agent's seats are changed here, through the registry's own door, by an operator-tier session; every other field is written by boss tenant publish."
  />

  {#if roster.kind === 'loading'}
    <p class="ag-quiet">Reading the agents registry…</p>
  {:else if roster.kind === 'failed'}
    <p class="ag-fail load-failed">
      The agents registry did not answer: {roster.error}. An unreachable read is not an empty registry, so
      nothing is drawn.
    </p>
  {:else if roster.data.length === 0}
    <p class="ag-notice">The registry holds no agent.</p>
  {:else}
    {#each roster.data as a (a.id)}
      {@const d = details[a.id]}
      {@const sent = seating[a.id]}
      <section class="ag-card" aria-label={a.name}>
        <h2>{a.name} <span class="mono ag-id">{a.id}</span></h2>
        <dl class="ag-facts">
          <dt>Aliases</dt>
          <dd class="mono">{a.aliases.length === 0 ? 'none' : a.aliases.join(', ')}</dd>
          <dt>Role</dt>
          <dd class="mono">{a.role ?? 'holds no role'}</dd>
          <dt>Department</dt>
          <dd class="mono">{a.department ?? 'not declared'}</dd>
          <dt>Default model</dt>
          <dd class="mono">{a.defaultModel ?? 'not declared'}</dd>
          <dt>Model class</dt>
          <dd class="mono">{a.modelClass ?? 'records none'}</dd>
          <dt>Hourly budget</dt>
          <dd class="mono">{budgetText(a.hourlyBudgetUsdMicros)}</dd>
          <dt>Seats</dt>
          <dd class="mono">{seatsText(a.stations)}</dd>
          <dt>Open steps held</dt>
          <dd class="mono">
            {#if !d}
              reading…
            {:else if d.held.kind === 'failed'}
              <span class="load-failed">did not answer: {d.held.error}</span>
            {:else if d.held.kind === 'ready'}
              {heldText(d.held.data)}
            {/if}
          </dd>
          {#if d && d.runs.kind === 'ready' && d.runs.data.length > 0}
            <dt>Effort in these runs</dt>
            <dd class="mono">{effortsRun(d.runs.data)}</dd>
          {/if}
        </dl>

        <form
          class="ag-seat"
          onsubmit={(e) => {
            e.preventDefault();
            void seat(a);
          }}
        >
          <label>
            Seat at these stations, in the order it serves them
            <input
              name="stations"
              class="mono"
              value={drafts[a.id] ?? a.stations.join(', ')}
              oninput={(e) => (drafts = { ...drafts, [a.id]: e.currentTarget.value })}
              placeholder="e.g. algedonic"
            />
          </label>
          <button type="submit" disabled={sent === 'sending'}>Seat {a.id}</button>
          <p class="ag-quiet">
            Changes this row's seats and nothing else, through the agents registry's seat door, and only
            while the registry still holds the seats shown above; signed by your session. The door admits
            an operator-tier session only: yours after Operator access with your passkey. An empty list
            unseats it.
          </p>
        </form>
        {#if sent === 'sending'}
          <p class="ag-quiet">Sending to the agents registry…</p>
        {:else if sent?.kind === 'applied'}
          <div class="ag-seat-result">
            <p class="mono">Before: {seatsText(sent.before.stations)}</p>
            <p class="mono">After: {seatsText(sent.after.stations)}</p>
            {#if sent.changes.length === 0}
              <p>The registry already held these seats; the door changed nothing.</p>
            {:else}
              <p>The door changed, recorded as jobs.agent.updated naming you:</p>
              <ul class="mono">
                {#each sent.changes as c (c.field)}
                  <li>{changeText(c)}</li>
                {/each}
              </ul>
            {/if}
          </div>
        {:else if sent?.kind === 'refused'}
          <p class="ag-fail load-failed ag-seat-refused">
            The agents registry refused this (HTTP {sent.status}): {sent.why}. Nothing changed.
          </p>
        {:else if sent?.kind === 'stale'}
          <p class="ag-fail load-failed ag-seat-refused">
            Nothing changed: the seats moved since this page read them — {sent.why}.
            {#if sent.held.kind === 'ready'}
              The card now shows what the registry holds; decide again on that.
            {:else}
              The registry could not be read again ({sent.held.error}); reload before trying again.
            {/if}
          </p>
        {:else if sent?.kind === 'unknown'}
          <p class="ag-fail load-failed ag-seat-refused">
            Whether this landed is unknown — {sent.why}.
            {#if sent.held.kind === 'ready'}
              Read again after it, the registry holds: {seatsText(sent.held.data.stations)}.
            {:else}
              The registry could not be read again ({sent.held.error}); reload before trying again.
            {/if}
          </p>
        {/if}

        {#if !d}
          <p class="ag-quiet">Reading this agent's runs…</p>
        {:else if d.runs.kind === 'failed'}
          <p class="ag-fail load-failed">The agent-run record did not answer: {d.runs.error}.</p>
        {:else if d.runs.kind === 'ready' && d.runs.data.length === 0}
          <p class="ag-quiet">No finished run is recorded for this id.</p>
        {:else if d.runs.kind === 'ready'}
          <h3>
            {d.runs.data.length === RUN_WINDOW
              ? `The newest ${RUN_WINDOW} finished runs`
              : `${d.runs.data.length} finished run${d.runs.data.length === 1 ? '' : 's'}`}
          </h3>
          <div class="ag-tbl">
            <table class="ag-table">
              <thead>
                <tr><th>Finished</th><th>Outcome</th><th>Effort</th><th>Model</th><th class="num">Cost</th><th>Work</th></tr>
              </thead>
              <tbody>
                {#each d.runs.data as r (r.runId)}
                  <tr>
                    <td><EntityLink kind="job" id={r.runId} label={when(r.finishedAt)} mono /></td>
                    <td>{r.outcome ?? 'not recorded'}</td>
                    <td>{r.effort ?? 'not recorded'}</td>
                    <td class="mono">{r.model ?? 'not recorded'}</td>
                    <td class="num mono">{costText(r)}</td>
                    <td>
                      {#if r.packet}
                        <EntityLink kind="job" id={r.packet} label={r.branch ?? 'the packet it ran'} mono />
                      {:else}
                        {r.branch ?? '—'}
                      {/if}
                    </td>
                  </tr>
                {/each}
              </tbody>
            </table>
          </div>
        {/if}
      </section>
    {/each}

    <p class="ag-footnote">
      The registry records no manager for an agent, so no reports-to line is drawn. Open steps held counts the
      ready and active steps assigned to the id or any alias. Runs are finished runs from the agent-run record,
      each linked to its own packet; a run still open is on the alarms board's
      <Link to={href(regionHref('shop-floor'))}>shop floor</Link>.
    </p>
  {/if}
</div>

<style>
  .ag-root { padding: 0 32px 32px; }
  .ag-quiet { color: var(--static); font-size: 13px; }
  .ag-fail {
    color: var(--warn); border: 1px solid var(--warn);
    padding: 8px 12px; font-size: 13px;
  }
  .ag-notice {
    color: var(--fog); border: 1px solid var(--hairline);
    padding: 8px 12px; font-size: 13px; max-width: 90ch;
  }
  .mono { font-family: var(--font-mono); font-variant-numeric: tabular-nums; }
  .ag-card { border: 1px solid var(--hairline); padding: 12px 16px; margin-top: 16px; }
  .ag-card h2 { font-size: 16px; font-weight: 600; margin: 0 0 8px; }
  .ag-card h3 { font-size: 13px; font-weight: 500; color: var(--static); margin: 14px 0 4px; }
  .ag-id { font-size: 12px; font-weight: 400; color: var(--static); margin-left: 6px; }
  .ag-facts {
    display: grid; grid-template-columns: max-content 1fr;
    gap: 4px 16px; margin: 0; font-size: 13px;
  }
  .ag-facts dt { color: var(--static); }
  .ag-facts dd { margin: 0; overflow-wrap: anywhere; }
  .ag-seat { margin-top: 12px; font-size: 13px; }
  .ag-seat label { display: flex; flex-wrap: wrap; gap: 4px 8px; align-items: center; color: var(--static); }
  .ag-seat input { min-width: 16ch; flex: 1 1 24ch; max-width: 60ch; }
  .ag-seat button { margin-top: 6px; }
  .ag-seat-result { border: 1px solid var(--hairline); padding: 8px 12px; margin-top: 8px; font-size: 13px; }
  .ag-seat-result p, .ag-seat-result ul { margin: 2px 0; }
  .ag-tbl { overflow-x: auto; }
  .ag-table { width: 100%; border-collapse: collapse; font-size: 12px; }
  .ag-table th { text-align: left; font-weight: 500; color: var(--static); padding: 4px 8px; border-bottom: 1px solid var(--hairline); }
  .ag-table td { padding: 6px 8px; border-bottom: 1px solid var(--hairline); white-space: nowrap; }
  .ag-table .num { text-align: right; }
  .ag-footnote { color: var(--text-faint); font-size: 12px; max-width: 90ch; margin-top: 20px; }
</style>
