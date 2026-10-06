<script lang="ts">
  // THE TOP BOARD (design ea906603, car 4 — backlog 74569e94) in the HUD
  // frame of design 00774ca8. A band above the map, four rows of things
  // to act on: what OUTRANKS REGULAR ORDER, what NEEDS YOU, what is NEXT
  // UP (a departures board), and the MACHINES. The three-thirds flow rows
  // it replaced are removed, not moved (David, Q5, 2026-09-27). hud.ts
  // turns each field into a picture and this file only draws them.
  //
  // FIXED (00774ca8 decision 6): the frame shows the same rows in the
  // same places whatever is selected — it answers about the whole system,
  // so it does not follow the zoom. Below the rows the contextual strip
  // keeps its height reserved, so the map under it never jumps.
  //
  // EACH ROW FAILS ALONE (8c7c2f4b's intent). Every row's picture is
  // computed and drawn inside its own <svelte:boundary>: a row that
  // throws — a duplicate machine key, a shape this client cannot draw —
  // becomes that row's unread `?` with a failure line naming the error,
  // and the other rows and the frame keep their shape. The row tries
  // again on the next read, so a one-off bad payload does not hold the
  // row dark after the server has moved on.
  //
  // THE MARKS ARE ENAMEL PLATES (00774ca8 decision 7): the shared
  // `.plate .plate-troubled` for what outranks regular order and a failed
  // machine. Colours are the map's --map-* tokens only
  // (map-palette.test.ts reads this file).
  import { onMount, type Snippet } from 'svelte';
  import { navigate } from '@boss/web-kit/nav';
  import { safeLinkHref } from '@boss/web-kit/links';
  import type { Remote } from '../../data/remote';
  import type { Regions } from './regions';
  import { NOT_IN_SCOPE } from '../../policy/withheld';
  import {
    headerOf,
    machineHref,
    machinesBoard,
    needsYouBoard,
    nextUpBoard,
    outranksBoard,
    type Figure,
    type LastGood,
    type NeedsYou,
  } from './hud';

  type Props = Readonly<{
    /** The latest regions read, landed at `readAt`. */
    read: Remote<Regions>;
    readAt: number | null;
    /** The newest read that succeeded: its time for the failure header —
     *  never its values. */
    lastGood: LastGood | null;
    /** The viewer's own queue — /api/jobs/assignments?for=me. */
    needs: Remote<NeedsYou>;
    /** What the contextual strip shows for the current selection. */
    strip?: Snippet;
  }>;
  let { read, readAt, lastGood, needs, strip }: Props = $props();

  // The read's age and the countdowns tick on their own clock — the page
  // polls every 10s, and "in 7m" that never moves would itself be a
  // stale figure.
  let now = $state(Date.now());
  onMount(() => {
    const t = setInterval(() => (now = Date.now()), 1000);
    return () => clearInterval(t);
  });

  const header = $derived(headerOf(read, readAt, now, lastGood));

  /** The failed rows' resets, called when the next read lands. */
  let resets: Array<() => void> = [];
  $effect(() => {
    void readAt;
    const pending = resets;
    resets = [];
    for (const reset of pending) reset();
  });

  function open(e: MouseEvent, href: string): void {
    if (e.metaKey || e.ctrlKey || e.shiftKey || e.button !== 0) return;
    e.preventDefault();
    navigate(href);
  }

  const message = (error: unknown): string => (error instanceof Error ? error.message : String(error));
</script>

{#snippet fig(f: Figure)}
  {#if f.kind === 'zero'}
    <span class="fig" data-fig="zero">0</span>
  {:else if f.kind === 'value'}
    <span class="fig" class:plate={f.troubled} class:plate-troubled={f.troubled} data-fig="value">{f.text}</span>
  {:else if f.kind === 'floor'}
    <span class="fig" data-fig="floor" title={f.why}
      ><span class:plate={f.troubled} class:plate-troubled={f.troubled}>{f.text}</span><span class="q-band">?</span></span>
  {:else}
    <span class="fig q-housing" data-fig="unread" title={f.why}>?</span>
  {/if}
{/snippet}

<!-- An unread row: `?` in the dashed housing, and the reason said. -->
{#snippet unreadLine(why: string)}
  <div class="line" data-state="unread">
    <span class="fig q-housing" data-fig="unread" title={why}>?</span>
    <span class="hud-why">{why}</span>
  </div>
{/snippet}

<!-- ONE ROW: its label, and its body under its own boundary. -->
{#snippet row(name: string, label: string, body: Snippet)}
  <div class="hud-row" data-row={name}>
    <div class="hud-label">{label}</div>
    <div class="hud-cell">
      <svelte:boundary onerror={(_, reset) => resets.push(reset)}>
        {@render body()}
        {#snippet failed(error)}
          <div class="line" data-state="failed">
            <span class="fig q-housing" data-fig="unread" title={message(error)}>?</span>
            <span class="hud-why load-failed">This row cannot be drawn — {message(error)}</span>
          </div>
        {/snippet}
      </svelte:boundary>
    </div>
  </div>
{/snippet}

{#snippet outranks()}
  {@const b = outranksBoard(read)}
  {#if b.kind === 'unread'}
    {@render unreadLine(b.why)}
  {:else}
    <div class="hud-board" data-state={b.kind}>
      <div class="line hud-count">{@render fig(b.count)}</div>
      {#if b.kind === 'empty'}
        <p class="hud-words">{b.words}</p>
      {:else}
        <ul class="hud-lines">
          {#each b.lines as l (l.id)}
            <li data-outrank={l.id}>
              <a href={safeLinkHref(l.href)} onclick={(e) => open(e, l.href)}>
                <span class="plate plate-troubled hud-pri">{l.priority}</span>
                <span class="hud-proto">{l.protocol}</span>
                <span class="hud-t">{l.title}</span>
                <span class="hud-sub">{l.age}</span>
                <span class="hud-sub">{l.where === null ? l.at : `${l.at} · ${l.where}`}</span>
              </a>
            </li>
          {/each}
          {#if b.more > 0}<li class="hud-more">… {b.more} more</li>{/if}
        </ul>
      {/if}
    </div>
  {/if}
{/snippet}

{#snippet needsYou()}
  {@const b = needsYouBoard(needs, now)}
  {#if b.kind === 'unread'}
    {@render unreadLine(b.why)}
  {:else}
    <div class="hud-board" data-state={b.kind}>
      <div class="line hud-count">{@render fig(b.count)}<span class="hud-sub">for {b.whom}</span></div>
      {#if b.kind === 'empty'}
        <p class="hud-words">{b.words}</p>
      {:else}
        <ul class="hud-lines">
          {#each b.lines as l (l.id)}
            <li data-need={l.id}>
              <a href={safeLinkHref(l.href)} onclick={(e) => open(e, l.href)}>
                <span class="hud-kind">{l.step}</span>
                <span class="hud-proto">{l.protocol}</span>
                <span class="hud-t">{l.title}</span>
                <span class="hud-sub">{l.stepTitle}</span>
                {#if l.agentTakes}<span class="plate plate-quiet hud-pri" data-agent-takes>an agent takes this</span>{/if}
                {#if l.priority !== null}<span class="plate plate-troubled hud-pri">{l.priority}</span>{/if}
                <span class="hud-sub">{l.age}</span>
              </a>
            </li>
          {/each}
          {#if b.more > 0}
            <li class="hud-more"><a href="/ux/me" onclick={(e) => open(e, '/ux/me')}>… {b.more} more on My Day</a></li>
          {/if}
        </ul>
      {/if}
    </div>
  {/if}
{/snippet}

<!-- THE DEPARTURES BOARD: what, then the time in the viewer's zone and
     the time left, in mono columns. -->
{#snippet nextUp()}
  {@const b = nextUpBoard(read, now)}
  {#if b.kind === 'unread'}
    {@render unreadLine(b.why)}
  {:else if b.kind === 'empty'}
    <div class="hud-board" data-state="empty"><p class="hud-words">{b.words}</p></div>
  {:else}
    <div class="hud-board hud-departures" data-state="rows">
      <div class="dep dep-head"><span class="dep-source"></span><span></span><span class="dep-time">time ({b.zone})</span><span class="dep-in">in</span></div>
      {#each b.lines as l, i (i)}
        <div class="dep" data-next={l.kind} data-state={l.state} title={l.source}>
          <span class="dep-source">{l.kind}</span>
          {#if l.state === 'unread'}
            <!-- A dark source is its own line, `?` and why — the rest of
                 the board still reads. -->
            <div class="dep-what">{@render unreadLine(l.why ?? '')}</div><span></span><span></span>
          {:else if l.state === 'withheld'}
            <!-- Withheld by policy scope (bd506215): the row as itself,
                 said neutral — no `?`, which is a failure's picture. -->
            <div class="dep-what" title={l.why ?? ''}>
              <span class="dep-title">{l.title}</span>
              <span class="dep-basis">{NOT_IN_SCOPE}</span>
            </div>
            <span class="dep-time dep-none">—</span><span></span>
          {:else}
            <div class="dep-what">
              <span class="dep-title">{l.title}</span>
              {#if l.basis !== ''}<span class="dep-basis">{l.basis}</span>{/if}
            </div>
            {#if l.state === 'timed'}
              <!-- An estimate wears "~" on both its time and its countdown. -->
              <span class="dep-time">{l.estimate ? '~' : ''}{l.time}</span>
              <span class="dep-in" class:late={l.countdown?.endsWith('late')}
                >{l.estimate ? '~' : ''}{l.countdown?.replace(/^in /, '')}</span>
            {:else}
              <span class="dep-time dep-none">no time</span><span></span>
            {/if}
          {/if}
        </div>
      {/each}
    </div>
  {/if}
{/snippet}

{#snippet machines()}
  {@const m = machinesBoard(read)}
  <div class="hud-board" data-machines>
    <div class="line hud-machine-line">
      {@render fig(m.failed)}<span>failed</span><span class="dot">·</span>{@render fig(m.unjudged)}<span
        >unjudged of</span>{@render fig(m.total)}
    </div>
    {#if m.listed.length > 0}
      <ul class="hud-listed">
        {#each m.listed as x (`${x.region}:${x.id}`)}
          <li>
            <a href={safeLinkHref(machineHref(x))} title={x.why} data-machine={x.id} onclick={(e) => open(e, machineHref(x))}
              >{`${x.state === 'failed' ? 'failed' : 'unjudged'} · ${x.region} · ${x.name}`}</a>
          </li>
        {/each}
      </ul>
    {/if}
  </div>
{/snippet}

<section class="hud" data-hud data-read={header.read} aria-label="The whole system">
  <header class="hud-head">
    <span class="hud-title">The whole system</span>
    <span class="hud-age" class:failed={header.read === 'failed'}>{header.text}</span>
  </header>
  <div class="hud-body">
    {@render row('outranks', 'Outranks regular order', outranks)}
    {@render row('needs-you', 'Needs you', needsYou)}
    {@render row('next-up', 'Next up', nextUp)}
    {@render row('machines', 'Machines', machines)}
  </div>
  <div class="hud-strip" data-strip>
    {#if strip}{@render strip()}{/if}
  </div>
</section>

<style>
  .hud { border: 1px solid var(--map-rule); border-radius: var(--radius); margin: 0 0 16px;
    background: var(--map-surface); color: var(--map-ink); font-size: 13px; }
  .hud-head { display: flex; justify-content: space-between; gap: 4px 12px; flex-wrap: wrap;
    padding: 8px 12px; border-bottom: 1px solid var(--map-rule); }
  .hud-title { font-family: var(--font-mono); font-size: 11px; letter-spacing: var(--ls-nav);
    text-transform: uppercase; color: var(--map-muted); }
  .hud-age { font-family: var(--font-mono); font-size: 11px; color: var(--map-muted); }
  .hud-age.failed { color: var(--map-bad-ink); }
  .hud-row { display: grid; grid-template-columns: 180px minmax(0, 1fr); align-items: start;
    gap: 4px 16px; padding: 7px 12px; }
  .hud-row + .hud-row { border-top: 1px solid var(--map-rule); }
  /* The row names are the board's station plates: mono, uppercase. */
  .hud-label { font-family: var(--font-mono); font-size: 11px; letter-spacing: var(--ls-nav);
    text-transform: uppercase; color: var(--map-muted); padding-top: 2px; }
  .hud-cell { min-width: 0; }
  .line { display: flex; flex-wrap: wrap; align-items: baseline; column-gap: 5px; }
  .hud-sub, .hud-why { font-size: 11.5px; color: var(--map-muted); }
  .hud-words { margin: 2px 0 0; color: var(--map-ink); }
  .dot { color: var(--map-muted); }
  .hud-lines, .hud-listed { list-style: none; margin: 4px 0 0; padding: 0; }
  .hud-lines li { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .hud-lines li + li { margin-top: 2px; }
  .hud-lines a, .hud-listed a { color: var(--map-link); text-decoration: none; }
  .hud-lines a:hover .hud-t, .hud-listed a:hover, .hud-more a:hover { text-decoration: underline; }
  .hud-lines a > span + span { margin-left: 6px; }
  .hud-t { color: var(--map-ink); }
  .hud-proto, .hud-kind { font-family: var(--font-mono); font-size: 11px; color: var(--map-muted); }
  .hud-pri { font-family: var(--font-mono); font-size: 10.5px; text-transform: uppercase; }
  .hud-more { font-size: 11.5px; color: var(--map-muted); }
  .hud-listed { font-size: 11.5px; }
  .hud-listed li { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .hud-machine-line { margin-top: 2px; }
  /* THE DEPARTURES BOARD: a mono grid — source, what, time, time left. */
  .dep { display: grid; grid-template-columns: 110px minmax(0, 1fr) 92px 64px; column-gap: 12px;
    align-items: baseline; font-family: var(--font-mono); font-size: 12px; }
  .dep + .dep { margin-top: 2px; }
  .dep-head { font-size: 10.5px; letter-spacing: var(--ls-nav); text-transform: uppercase; color: var(--map-muted);
    white-space: nowrap; }
  .dep-source { color: var(--map-muted); text-transform: uppercase; font-size: 10.5px; letter-spacing: var(--ls-nav); }
  .dep-what { font-family: var(--font-body); font-size: 13px; min-width: 0; }
  .dep-title, .dep-basis { display: block; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  /* The basis — what the time rests on, or why there is none — is the
     row's small print. */
  .dep-basis { font-size: 11px; color: var(--map-muted); }
  .dep-none { color: var(--map-muted); font-size: 10.5px; }
  .dep-time, .dep-in { text-align: right; font-variant-numeric: tabular-nums; }
  .dep-in.late { color: var(--map-bad-ink); }
  /* The contextual strip: empty until something is selected, and its
     height reserved either way (decision 6). */
  .hud-strip { min-height: 28px; border-top: 1px solid var(--map-rule); padding: 4px 12px;
    font-size: 12px; color: var(--map-muted); }

  /* THE PICTURES (00774ca8 decision 5). A zero and a value are plain; a
     floor's uncounted part and an unread figure are the map's unknown:
     a broken outline, muted — never a zero, never blank. */
  .fig { font-variant-numeric: tabular-nums; }
  .q-housing, .q-band { display: inline-block; min-width: 1.4em; padding: 0 4px; text-align: center;
    border: 1px dashed var(--map-muted); border-radius: var(--radius); color: var(--map-muted);
    font-family: var(--font-mono); font-size: 11px; line-height: 15px; }
  .q-band { margin-left: 3px; }

  /* Phone: each row stacks its label over its body, and the departures
     drop the source column, so nothing scrolls sideways. */
  @media (max-width: 720px) {
    .hud-row { grid-template-columns: minmax(0, 1fr); gap: 2px; }
    .dep { grid-template-columns: minmax(0, 1fr) 80px 56px; }
    .dep-source { display: none; }
  }
</style>
