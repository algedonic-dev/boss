<script lang="ts">
  // Yard status — "what is the yard doing, and why?" answered from live
  // system-of-record data (the-cluster-is-the-system.md Phase 0). Where
  // the Train Yard board watches trains move, this answers the
  // operational question an operator used to SSH for: where does each
  // train sit, and if one is stuck, WHY. The block reason that used to
  // live buried in a step's metadata (four hours wedged on 2026-09-02)
  // is a first-class line here, and the boarding predicate is rendered
  // from the LIVE cadence rows — no threshold is baked into this panel.
  //
  // A PANEL, NOT A PAGE (design e765b3fc, car N3, 2026-09-25). It was
  // /it/operate/yard-status, an Operate tab; its lanes are three
  // stations' detail now, each drawn in that station's panel below the
  // Department Map: a train's phase, block and ETA and the recent trains
  // are the TRACK's, the dock's cars, its boarding sentence and the held
  // cars are the DOCK's, and the stranded and held greens are the
  // GARAGE's (the garage region claims both). `part` names the station;
  // every lane the page drew is drawn by exactly one of them, so nothing
  // the page showed is lost.
  import { onMount } from 'svelte';
  import StatusChip from '@boss/web-kit/ui/StatusChip.svelte';
  import { formatRelative } from '@boss/web-kit/ui/date';
  import type { Remote } from '../../data/remote';
  import {
    RELEASE_ROLE,
    blockLabel,
    etaDetail,
    etaReading,
    fetchYardStatus,
    journeyText,
    phaseLabel,
    redsCell,
    releasableHeld,
    releaseBody,
    safeSaysNo,
    shortSha,
    trainTone,
    type HeldCar,
    type YardStatus,
  } from './yard-status';
  import { safeLinkHref } from '@boss/web-kit/links';
  import { session } from '@boss/web-kit/session/session.svelte';

  let { part }: Readonly<{ part: 'track' | 'dock' | 'garage' }> = $props();

  let status = $state<Remote<YardStatus>>({ kind: 'loading' });
  // One clock for every relative stamp on the page, taken when the data
  // arrived — formatRelative takes `now` explicitly, no hidden wallclock.
  let loadedAt = $state<Date>(new Date());

  async function refresh(): Promise<void> {
    status = await fetchYardStatus();
    loadedAt = new Date();
  }

  onMount(() => {
    void refresh();
    // 15s: fast enough that a newly-stuck train shows promptly, slow
    // enough for a guest-safe read.
    const t = setInterval(() => void refresh(), 15_000);
    return () => clearInterval(t);
  });

  const outcomeTone = (o: string): 'ok' | 'warn' | 'muted' =>
    o === 'arrived' ? 'ok' : o === 'cancelled' ? 'warn' : 'muted';

  // RELEASE, FROM THE ROW (backlog ed8e5c85, David 2026-10-09: "Copy
  // and pasting a whole bunch of releases is inefficient"). Each held
  // car shows what its release rests on, and a Release control calls
  // the ONE door `boss release --review` also calls:
  // POST /api/jobs/{car}/release. The page judges nothing. It offers the
  // button where the SERVER's decision says releasable, and it shows the
  // door's own words when the door refuses. The control is offered to a
  // platform-admin session (affordance, not the gate). The record names
  // that session's actor, and the stamp is the approval (design
  // 1ce67f7e), so there is no second prompt.
  const privileged = $derived(
    session.value.kind === 'ready' && session.value.user.role === RELEASE_ROLE,
  );
  /** Which confirmation is open: one car's id, `all`, or none. */
  let confirming = $state<string | null>(null);
  let releasing = $state(false);
  let released = $state<Readonly<Record<string, Readonly<{ ok: boolean; text: string }>>>>({});

  async function postRelease(h: HeldCar): Promise<Readonly<{ ok: boolean; text: string }>> {
    const body = h.release ? releaseBody(h.release) : null;
    if (body === null) return { ok: false, text: 'no review run to release on' };
    try {
      const r = await fetch(`/api/jobs/${encodeURIComponent(h.id)}/release`, {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify(body),
      });
      const text = (await r.text()).trim();
      return r.ok
        ? { ok: true, text: 'released — it boards at the next tick' }
        : { ok: false, text: `HTTP ${r.status}${text ? ` — ${text}` : ''}` };
    } catch (e) {
      return { ok: false, text: e instanceof Error ? e.message : String(e) };
    }
  }

  /** One at a time, in the order listed, so each answer is recorded
   *  against its car before the next is asked. */
  async function releaseCars(cars: readonly HeldCar[]): Promise<void> {
    releasing = true;
    for (const h of cars) {
      const answer = await postRelease(h);
      released = { ...released, [h.id]: answer };
    }
    releasing = false;
    confirming = null;
    await refresh();
  }
</script>

<div class="ys-root" data-yard-status={part}>
  {#if status.kind === 'loading'}
    <p class="ys-quiet">Reading the yard status…</p>
  {:else if status.kind === 'failed'}
    <p class="ys-fail load-failed">
      The yard status did not answer: {status.error}. This panel refuses to
      guess — an unreachable read is not an empty yard.
    </p>
  {:else}
    {@const s = status.data}

    {#if part === 'track'}
    <!-- IN FLIGHT: where each train sits, and the block if stuck -->
    <div class="ys-section">IN FLIGHT</div>
    {#if s.trains.length === 0}
      <p class="ys-quiet">No trains in flight.</p>
    {:else}
      <div class="ys-trains">
        {#each s.trains as t (t.id)}
          {@const eta = etaReading(t.eta)}
          <div class="ys-train" class:blocked={!!t.block}>
            <div class="ys-train-head">
              <span class="ys-train-title">{t.title}</span>
              <StatusChip value={phaseLabel(t.phase)} tone={trainTone(t)} />
              {#if t.car_count > 0}
                <span class="ys-cars">{t.car_count} car{t.car_count === 1 ? '' : 's'}</span>
              {/if}
              {#if safeLinkHref(t.pr_url)}
                <a class="ys-pr" href={safeLinkHref(t.pr_url)} target="_blank" rel="noreferrer">PR ↗</a>
              {:else if t.pr_url}
                <!-- Not http(s): drawn as text, never a link (4f1f7698). -->
                <span class="ys-pr" title={t.pr_url}>PR</span>
              {/if}
            </div>
            {#if t.at_step}
              <div class="ys-at">at: {t.at_step}</div>
            {/if}
            <!-- WHEN IT LANDS. The gates panel below has carried a
                 measured `typical_seconds` for months; trains had no
                 equivalent, so reading whether a 20-minute transit was
                 normal meant asking a human. The figure is measured from
                 ARRIVED trains only (a refused board opens and cancels a
                 pr-train, and those are the majority), it names the leg
                 it covers, and with too little history it says so rather
                 than guessing. -->
            <div class="ys-eta" class:late={eta.tone === 'err'} class:off={eta.tone === 'muted'} title={etaDetail(t.eta)}>
              {eta.text}
              {#if t.eta.kind === 'estimate'}
                <span class="ys-eta-leg">{t.eta.leg} · {t.eta.sample_size} arrivals</span>
              {:else}
                <span class="ys-eta-leg">{t.eta.reason}</span>
              {/if}
            </div>
            {#if t.block}
              <!-- The buried fact, surfaced. -->
              <div class="ys-block">{blockLabel(t.block)}</div>
              {#if t.block.kind === 'deploy-blocked' && t.block.since}
                <div class="ys-block-since">
                  blocked since {formatRelative(t.block.since, loadedAt)}
                </div>
              {/if}
              {#if t.block.kind === 'stalled'}
                <div class="ys-block-since">
                  stalled since {formatRelative(t.block.since, loadedAt)}
                </div>
              {/if}
            {/if}
          </div>
        {/each}
      </div>
    {/if}
    {/if}

    {#if part === 'dock'}
    <!-- THE DOCK + the boarding predicate, from the live registry -->
    <div class="ys-section">THE DOCK</div>
    <p class="ys-boarding" title="Read from the live cadence_rules — not a constant in this page.">
      {s.boarding.summary}
    </p>
    {#if s.dock.length === 0}
      <p class="ys-quiet">No cars parked and ready to board.</p>
    {:else}
      <table class="ys-table">
        <thead>
          <tr><th>car</th><th>branch</th><th>parked</th><th>reds</th></tr>
        </thead>
        <tbody>
          {#each s.dock as c (c.id)}
            <tr>
              <td>{c.title}</td>
              <td class="ys-mono">{c.branch ?? '—'}</td>
              <td class="ys-mono ys-dim">{c.parked_since}</td>
              <!-- Strikes: one red behind a car is the state in which the
                   NEXT red holds it out, so it takes the warn token the
                   page already uses; a clean car's cell is blank (cb6714de). -->
              <td class="ys-mono ys-reds">{redsCell(c.red_trains)}</td>
            </tr>
          {/each}
        </tbody>
      </table>
    {/if}

    <!-- HELD CARS: standing on the dock, unable to board, with the
         reason written on the review step. The server has stated this
         lane since #367 and the parser has read it; the page drew only
         the held GREENS below, so a car held out for two reds — the one
         state an operator must act on — showed on the yard map alone
         (b6522ff9). Always drawn, empty state included: this lane going
         quiet is exactly how it went unread. -->
    <div class="ys-section" id="held-cars">HELD CARS</div>
    {#if s.held_cars.length === 0}
      <p class="ys-quiet">none held</p>
    {:else}
      {@const ready = releasableHeld(s.held_cars)}
      <p class="ys-quiet">
        Parked, gated green, and held out — released on a review that says release at the
        head the hold was placed at.
      </p>
      <!-- RELEASE EVERY REVIEWED CAR: the rows the server says it would
           release, listed, then one confirmation. Each line carries the
           hold's reason and the reviewer's SAFE line, and a reviewer's
           "no" is marked (review 4822764d N1). -->
      {#if privileged && ready.length > 0}
        <div class="ys-release-all">
          {#if confirming === 'all'}
            <p>Release these {ready.length} car{ready.length === 1 ? '' : 's'}, each on its review:</p>
            <ul class="ys-release-list">
              {#each ready as h (h.id)}
                {@const safe = h.release?.safe_line ?? null}
                <li class:ys-safe-no={safeSaysNo(safe)}>
                  <span class="ys-mono">{h.branch ?? h.id} — review {shortSha(h.release?.review ?? null)} at {shortSha(h.release?.head ?? null)}</span>
                  <div class="ys-dim">held: {h.reason}</div>
                  {#if safeSaysNo(safe)}<div class="ys-mismatch">the reviewer said NOT safe without David</div>{/if}
                  <div class="ys-safe">{safe ?? 'the reviewer wrote no SAFE line'}</div>
                </li>
              {/each}
            </ul>
            <button type="button" class="ys-btn ys-btn-go" disabled={releasing} onclick={() => void releaseCars(ready)}>
              {releasing ? 'Releasing…' : `Release ${ready.length}`}
            </button>
            <button type="button" class="ys-btn" disabled={releasing} onclick={() => (confirming = null)}>Cancel</button>
          {:else}
            <button type="button" class="ys-btn ys-btn-go" onclick={() => (confirming = 'all')}>
              Release every reviewed car ({ready.length})
            </button>
          {/if}
        </div>
      {/if}
      <table class="ys-table">
        <thead>
          <tr><th>branch</th><th>reason</th><th>review</th><th>reds</th><th>since</th><th></th></tr>
        </thead>
        <tbody>
          {#each s.held_cars as h (h.id)}
            {@const d = h.release ?? null}
            {@const answer = released[h.id]}
            <tr>
              <td class="ys-mono">{h.branch ?? '—'}</td>
              <td>{h.reason}</td>
              <td class="ys-review">
                {#if d === null}
                  <span class="ys-dim">no release decision stated</span>
                {:else}
                  <div>
                    <span class="ys-mono">{d.verdict ?? 'no verdict'}</span>
                    · reviewed <span class="ys-mono">{shortSha(d.reviewed_sha)}</span>
                    · held at <span class="ys-mono">{shortSha(d.head)}</span>
                    {#if d.head_matches === true}
                      <span class="ys-match">same head</span>
                    {:else if d.head_matches === false}
                      <span class="ys-mismatch">another head</span>
                    {/if}
                  </div>
                  {#if d.safe_line}<div class="ys-safe">{d.safe_line}</div>{/if}
                  {#if d.model}<div class="ys-dim" class:ys-reds={d.model_differs}>model: {d.model}</div>{/if}
                  {#if !d.releasable && d.refusal}<div class="ys-dim">not releasable: {d.refusal}</div>{/if}
                {/if}
              </td>
              <td class="ys-mono ys-reds">{redsCell(h.red_trains)}</td>
              <td class="ys-mono ys-dim">{h.parked_since}</td>
              <td class="ys-act">
                {#if answer}
                  <span class={answer.ok ? 'ys-match' : 'ys-mismatch'}>{answer.text}</span>
                {:else if privileged && d?.releasable}
                  {#if confirming === h.id}
                    <button type="button" class="ys-btn ys-btn-go" disabled={releasing} onclick={() => void releaseCars([h])}>
                      {releasing ? 'Releasing…' : 'Confirm'}
                    </button>
                    <button type="button" class="ys-btn" disabled={releasing} onclick={() => (confirming = null)}>Cancel</button>
                  {:else}
                    <button type="button" class="ys-btn ys-btn-go" onclick={() => (confirming = h.id)}>Release</button>
                  {/if}
                {/if}
              </td>
            </tr>
          {/each}
        </tbody>
      </table>
    {/if}
    {/if}

    {#if part === 'track'}
    <!-- RECENT: the last few trains and how they ended -->
    <div class="ys-section">RECENT TRAINS</div>
    {#if s.recent.length === 0}
      <p class="ys-quiet">No trains have closed recently.</p>
    {:else}
      <table class="ys-table">
        <thead>
          <tr><th>train</th><th>outcome</th><th>journey</th></tr>
        </thead>
        <tbody>
          {#each s.recent as r (r.id)}
            <tr>
              <td>{r.title}</td>
              <td><StatusChip value={r.outcome} tone={outcomeTone(r.outcome)} /></td>
              <td class="ys-mono ys-dim">{journeyText(r.journey_seconds)}</td>
            </tr>
          {/each}
        </tbody>
      </table>
    {/if}

    <!-- The thresholds this yard enforces, named from the policy row —
         the conductor's, so the track's. -->
    <div class="ys-footnote">
      {#if s.policy.stall_hours != null || s.policy.max_red_trains != null}
        Policy (train-conductor):
        {#if s.policy.stall_hours != null}a train stalls after {s.policy.stall_hours}h without
          progress{/if}{#if s.policy.stall_hours != null && s.policy.max_red_trains != null};
        {/if}{#if s.policy.max_red_trains != null}a car holds after {s.policy.max_red_trains} red
          trains{/if}.
      {:else}
        No delivery policy is configured; the conductor uses its compiled fallbacks.
      {/if}
    </div>
    {/if}

    {#if part === 'garage'}
    <!-- The recency lanes read a window; the server says when the record
         held more. Held greens are read by their hold, not by this window
         (2fa96d34), so the notice names the lanes it is about. -->
    {#if s.gate_runs?.truncated}
      <p class="ys-quiet">
        Slots, garage, limbo and stranded read the newest {s.gate_runs.window} gate-runs;
        held greens are listed from the whole record.
      </p>
    {/if}

    <!-- STRANDED: green gates no car claims (cheap signal) -->
    {#if s.stranded.length > 0}
      <div class="ys-section">STRANDED GREENS</div>
      <p class="ys-quiet">
        Gated green, never parked — so never on the dock, so they cannot board.
        Rescue (rebase + re-gate) or drop; never rebuild blind.
      </p>
      <ul class="ys-stranded">
        {#each s.stranded as g (g.branch)}
          <li class="ys-mono">{g.branch}</li>
        {/each}
      </ul>
    {/if}

    <!-- HELD: greens deliberately kept off the dock, with the
         operator's reason. Neutral, not amber: a brake on is not a gap. -->
    {#if s.held.length > 0}
      <div class="ys-section">HELD GREENS</div>
      <p class="ys-quiet">
        Gated green and held off the dock on purpose — waiting for what the reason says.
        Not stranded; nothing to rescue.
      </p>
      <ul class="ys-held">
        {#each s.held as h (h.branch)}
          <li><span class="ys-mono">{h.branch}</span> — held: {h.reason} <span class="ys-dim">(since {h.since})</span></li>
        {/each}
      </ul>
    {/if}
    {/if}
  {/if}
</div>

<style>
  .ys-root { padding: 0 0 8px; }
  .ys-section {
    font-family: var(--font-mono);
    font-size: 12px; letter-spacing: var(--ls-eyebrow);
    color: var(--signal); margin: 28px 0 8px;
    display: flex; align-items: center; gap: 12px;
  }
  .ys-section::after { content: ''; flex: 1; border-top: 1px solid var(--hairline); }
  .ys-quiet { color: var(--static); font-size: 13px; }
  .ys-fail {
    color: var(--warn);
    border: 1px solid var(--warn);
    padding: 8px 12px; font-size: 13px;
  }
  .ys-trains { display: flex; flex-direction: column; gap: 10px; }
  .ys-train {
    border: 1px solid var(--hairline);
    padding: 10px 12px; display: flex; flex-direction: column; gap: 4px;
  }
  .ys-train.blocked { border-color: var(--err); }
  .ys-train-head { display: flex; align-items: center; gap: 10px; flex-wrap: wrap; }
  .ys-train-title { font-weight: 600; }
  .ys-cars { color: var(--static); font-size: 12px; }
  .ys-pr {
    font-family: var(--font-mono); font-size: 12px;
    color: var(--signal); text-decoration: none; margin-left: auto;
  }
  .ys-at { color: var(--static); font-size: 13px; }
  .ys-eta { font-size: 13px; color: var(--ok); }
  .ys-eta.late { color: var(--err); font-weight: 600; }
  .ys-eta.off { color: var(--static); }
  .ys-eta-leg {
    color: var(--static);
    font-size: 12px;
    font-weight: 400;
    margin-left: 6px;
  }
  .ys-block {
    color: var(--err); font-weight: 600; font-size: 13px;
    font-family: var(--font-mono);
  }
  .ys-block-since { color: var(--static); font-size: 12px; }
  .ys-boarding {
    font-size: 13px; color: var(--fog);
    border-left: 2px solid var(--signal); padding-left: 10px; margin: 8px 0;
  }
  .ys-table { width: 100%; border-collapse: collapse; font-size: 13px; }
  .ys-table th {
    text-align: left; font-family: var(--font-mono);
    font-size: 11px; letter-spacing: 0.1em; text-transform: uppercase;
    color: var(--static); font-weight: 400;
    border-bottom: 1px solid var(--hairline); padding: 4px 12px 4px 0;
  }
  .ys-table td { padding: 6px 12px 6px 0; border-bottom: 1px solid var(--hairline); }
  .ys-mono { font-family: var(--font-mono); }
  .ys-dim { color: var(--static); }
  .ys-reds { color: var(--warn); }
  .ys-stranded { list-style: none; padding: 0; margin: 6px 0; display: flex; flex-direction: column; gap: 4px; }
  .ys-stranded li { color: var(--warn); font-size: 13px; }
  .ys-held { list-style: none; padding: 0; margin: 6px 0; display: flex; flex-direction: column; gap: 4px; }
  .ys-held li { color: var(--static); font-size: 13px; }
  .ys-footnote { color: var(--static); font-size: 12px; margin-top: 24px; max-width: 70ch; }
  .ys-review { font-size: 12px; max-width: 48ch; }
  .ys-safe { color: var(--fog); margin-top: 2px; }
  .ys-match { color: var(--ok); font-size: 12px; }
  .ys-mismatch { color: var(--warn); font-size: 12px; }
  .ys-act { white-space: nowrap; }
  .ys-release-all { margin: 8px 0 12px; font-size: 13px; }
  .ys-release-list { margin: 4px 0 8px; padding-left: 18px; }
  .ys-release-list li { margin-bottom: 6px; }
  .ys-release-list li.ys-safe-no { border-left: 3px solid var(--warn); padding-left: 6px; }
  .ys-btn {
    font-family: var(--font-mono); font-size: 12px;
    background: none; color: var(--fog);
    border: 1px solid var(--hairline); padding: 3px 10px; cursor: pointer;
    margin-right: 6px;
  }
  .ys-btn-go { color: var(--signal); border-color: var(--signal); }
  .ys-btn:disabled { opacity: 0.5; cursor: default; }
</style>
