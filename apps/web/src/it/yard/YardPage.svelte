<script lang="ts">
  // The train yard — the IT department's front door (departure-
  // board.md Q1, David's call: guest-visible, the IT app's landing).
  // A queue lens in the departure-board idiom: every row is a Job
  // the conductor writes; nothing here is new state. Reads are
  // audit-readonly-safe by construction.
  import { onMount } from 'svelte';
  import { fetchYard, type YardState, type TrainRow } from './yard';
  import PacketCard from '@boss/web-kit/ui/PacketCard.svelte';
  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';

  let yard = $state<YardState | null>(null);
  let loading = $state(true);

  onMount(() => {
    let cancelled = false;
    async function tick() {
      const y = await fetchYard();
      if (!cancelled && y) yard = y;
      loading = false;
    }
    tick();
    const t = setInterval(tick, 10_000);
    return () => {
      cancelled = true;
      clearInterval(t);
    };
  });

  function stampOf(t: TrainRow): string {
    if (t.status === 'ARRIVED' && t.deployed) return t.deployed;
    if (t.status === 'DEPARTED' && t.mergeRef) return `merged ${t.mergeRef}`;
    return '';
  }
</script>

<div class="theme-exec yard-root">
  <PageHeader
    eyebrow="IT · Forge line"
    title="The train yard"
    subtitle="Parked → boarded → departed → arrived — the pipeline's queues, live"
  />

  {#if loading}
    <div class="yard-empty">Reading the yard…</div>
  {:else if !yard}
    <div class="yard-empty">The yard is unreachable right now.</div>
  {:else}
    <div class="yard-section">01 — IN FLIGHT <span class="yard-n">{yard.inFlight.length}</span></div>
    {#if yard.inFlight.length === 0}
      <div class="yard-empty">No departures — nothing ready to board.</div>
    {:else}
      {#each yard.inFlight as t (t.id)}
        <div class="yard-trainblock">
          <div class="yard-trainhead">
            {#if t.live}<span class="yard-dot" title="in motion"></span>{/if}
            <span class="yard-trainname">{t.title}</span>
            <span class="yard-lamp" class:ok={t.lamp === 'green'} class:err={t.lamp === 'failing'} class:run={t.lamp === 'pending'}>
              {t.lamp === 'green' ? 'CI ✓' : t.lamp === 'failing' ? 'CI ✗' : 'CI …'}
            </span>
            <span class="yard-chip">{t.status}</span>
            <span class="yard-stamp">{stampOf(t)}</span>
          </div>
          <div class="yard-consist">
            {#if t.cars.length === 0}
              <span class="yard-empty">consist forming…</span>
            {:else}
              {#each t.cars as c (c.id)}
                <PacketCard card={c} size="consist" />
              {/each}
            {/if}
          </div>
        </div>
      {/each}
    {/if}

    <div class="yard-section">02 — LOADING DOCK <span class="yard-n">{yard.dock.length}</span></div>
    {#if yard.dock.length === 0}
      <div class="yard-empty">The dock is clear.</div>
    {:else}
      <div class="yard-dock">
        {#each yard.dock as c (c.id)}
          <PacketCard card={c} size="dock" />
        {/each}
      </div>
    {/if}

    <div class="yard-section">03 — RECENT ARRIVALS</div>
    <table class="yard-board">
      <thead><tr><th>Train</th><th>Consist</th><th>Arrival</th></tr></thead>
      <tbody>
        {#each yard.arrivals as t (t.id)}
          <tr><td>{t.title}</td><td>{t.cars.length} cars</td>
              <td class="yard-stamp">{t.deployed ?? (t.mergeRef ? `merged ${t.mergeRef}` : '—')}</td></tr>
        {/each}
      </tbody>
    </table>

    <div class="yard-flow">PARKED → BOARDED → <em>DEPARTED</em> → ARRIVED</div>
  {/if}
</div>

<style>
  .yard-root { padding: 0 32px 32px; }
  .yard-section {
    font-family: var(--font-mono, ui-monospace, monospace);
    font-size: 12px; letter-spacing: var(--ls-eyebrow, 0.3em);
    color: var(--signal, #5FD4A8); margin: 28px 0 8px;
    display: flex; align-items: center; gap: 12px;
  }
  .yard-section::after { content: ''; flex: 1; border-top: 1px solid var(--hairline, #2A3138); }
  .yard-n { color: var(--static, #7A838C); }
  .yard-board { width: 100%; border-collapse: collapse; background: var(--card, var(--ink, #12161C));
    border: 1px solid var(--hairline, #2A3138); font-size: 14px; }
  .yard-board th { font-family: var(--font-mono, ui-monospace, monospace); font-size: 11px;
    letter-spacing: var(--ls-nav, 0.14em); text-transform: uppercase; font-weight: 400;
    color: var(--static, #7A838C); text-align: left; padding: 8px 12px;
    border-bottom: 1px solid var(--hairline, #2A3138); }
  .yard-board td { padding: 7px 12px; border-bottom: 1px solid var(--hairline, #2A3138); }
  .yard-board tr:last-child td { border-bottom: none; }
  .yard-trainblock { border: 1px solid var(--hairline, #2A3138);
    background: var(--card, var(--ink, #12161C)); margin-bottom: 12px; }
  .yard-trainhead { display: flex; align-items: center; gap: 12px; padding: 9px 12px;
    border-bottom: 1px solid var(--hairline, #2A3138); font-size: 14px; }
  .yard-trainname { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis;
    white-space: nowrap; }
  /* The flatbed: consist cards sit on VOID so the packets read as
     cargo loaded onto the train, the same cards that wait in the dock. */
  .yard-consist { display: flex; flex-wrap: wrap; gap: 8px; padding: 10px 12px;
    background: var(--bg, var(--void, #0D1014)); }
  .yard-dock { display: grid; gap: 10px;
    grid-template-columns: repeat(auto-fill, minmax(250px, 1fr)); }
  .yard-chip { font-family: var(--font-mono, ui-monospace, monospace); font-size: 11px;
    letter-spacing: 0.1em; border: 1px solid var(--hairline, #2A3138); padding: 2px 8px; }
  .yard-lamp { font-family: var(--font-mono, ui-monospace, monospace); font-size: 11px;
    letter-spacing: 0.1em; border: 1px solid var(--hairline, #2A3138); padding: 2px 8px; }
  .yard-lamp.ok  { color: var(--ok, #4fb98a); border-color: var(--ok, #4fb98a); }
  .yard-lamp.err { color: var(--err, #e2685c); border-color: var(--err, #e2685c); }
  .yard-lamp.run { color: var(--warn, #d9a441); border-color: var(--warn, #d9a441); }
  .yard-dot { display: inline-block; width: 7px; height: 7px; border-radius: 50%;
    background: var(--signal, #5FD4A8); margin-right: 8px;
    animation: yard-pulse 1.4s ease-in-out infinite; }
  @keyframes yard-pulse { 50% { opacity: 0.35; } }
  @media (prefers-reduced-motion: reduce) { .yard-dot { animation: none; } }
  .yard-stamp { font-family: var(--font-mono, ui-monospace, monospace); font-size: 12px;
    color: var(--static, #7A838C); }
  .yard-empty { color: var(--static, #78716c); padding: 12px 0; font-size: 14px; }
  .yard-flow { font-family: var(--font-mono, ui-monospace, monospace); font-size: 11px;
    letter-spacing: var(--ls-nav, 0.14em); color: var(--static, #7A838C);
    border-top: 1px solid var(--hairline, #2A3138); margin-top: 28px; padding-top: 12px; }
  .yard-flow em { color: var(--signal, #5FD4A8); font-style: normal; }
</style>
