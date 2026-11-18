<script lang="ts">
  // NEEDS YOU — the human-required IT jobs, whoever holds them (David,
  // cef1f661): the approvals and the ops requests that wait on a person.
  // It was one row of the HUD frame's top board (design ea906603); R3 of
  // the transit-map retirement (backlog 23a4b4d7, conductor order
  // 20261108T1523Z) retired every other row and kept this one, because it
  // is the control David uses from /it. human-needs.ts turns the read into
  // a picture; this file only draws it. Colours are the --map-* tokens
  // (map-palette.test.ts reads this file).
  import { onMount } from 'svelte';
  import { navigate } from '@boss/web-kit/nav';
  import { safeLinkHref } from '@boss/web-kit/links';
  import type { Remote } from '../../data/remote';
  import { needsYouBoard, type Figure, type NeedsYou } from './human-needs';

  let { needs }: Readonly<{ needs: Remote<NeedsYou> }> = $props();

  // A job's age ticks on its own clock, so "today" turns at midnight.
  let now = $state(Date.now());
  onMount(() => {
    const t = setInterval(() => (now = Date.now()), 60_000);
    return () => clearInterval(t);
  });
  const b = $derived(needsYouBoard(needs, now));

  function open(e: MouseEvent, href: string): void {
    if (e.metaKey || e.ctrlKey || e.shiftKey || e.button !== 0) return;
    e.preventDefault();
    navigate(href);
  }
</script>

{#snippet fig(f: Figure)}
  {#if f.kind === 'zero'}
    <span class="fig" data-fig="zero">0</span>
  {:else if f.kind === 'value'}
    <span class="fig" data-fig="value">{f.text}</span>
  {:else if f.kind === 'floor'}
    <span class="fig" data-fig="floor" title={f.why}>{f.text}<span class="q">?</span></span>
  {:else}
    <span class="fig q" data-fig="unread" title={f.why}>?</span>
  {/if}
{/snippet}

<section class="needs" data-needs-you aria-label="Needs you">
  <h2 class="label">Needs you</h2>
  {#if b.kind === 'unread'}
    <p class="line" data-state="unread"><span class="fig q" data-fig="unread">?</span> <span class="sub">{b.why}</span></p>
  {:else}
    <p class="line" data-state={b.kind}>{@render fig(b.count)} <span class="sub">for {b.whom}</span></p>
    <!-- A cut answer says so in words beside its floor (backlog 97b5cf45). -->
    {#if b.cut !== null}<p class="sub" data-cut>cut: {b.cut}</p>{/if}
    {#if b.kind === 'empty'}
      <p class="words">{b.words}</p>
    {:else}
      <ul class="lines">
        {#each b.lines as l (l.id)}
          <li data-need={l.id}>
            <a href={safeLinkHref(l.href)} onclick={(e) => open(e, l.href)}>
              <span class="kind">{l.step}</span>
              <span class="kind">{l.protocol}</span>
              <span class="t">{l.title}</span>
              <span class="sub">{l.stepTitle}</span>
              {#if l.priority !== null}<span class="pri">{l.priority}</span>{/if}
              <span class="sub">{l.age}</span>
            </a>
          </li>
        {/each}
        {#if b.more > 0}
          <li class="sub"><a href="/ux/me" onclick={(e) => open(e, '/ux/me')}>… {b.more} more on My Day</a></li>
        {/if}
      </ul>
    {/if}
  {/if}
</section>

<style>
  .needs { border: 1px solid var(--map-rule); border-radius: var(--radius); margin: 0 0 16px; padding: 8px 12px;
    background: var(--map-surface); color: var(--map-ink); font-size: 13px; min-width: 0; }
  .label { margin: 0 0 4px; font-family: var(--font-mono); font-size: 11px; font-weight: 400; letter-spacing: var(--ls-nav);
    text-transform: uppercase; color: var(--map-muted); }
  .line { margin: 0; display: flex; flex-wrap: wrap; align-items: baseline; column-gap: 5px; }
  .sub { font-size: 11.5px; color: var(--map-muted); margin: 0; }
  .words { margin: 2px 0 0; }
  .lines { list-style: none; margin: 4px 0 0; padding: 0; }
  .lines li { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .lines li + li { margin-top: 2px; }
  .lines a { color: var(--map-link); text-decoration: none; }
  .lines a:hover .t { text-decoration: underline; }
  .lines a > span + span { margin-left: 6px; }
  .t { color: var(--map-ink); }
  .kind { font-family: var(--font-mono); font-size: 11px; color: var(--map-muted); }
  .pri { font-family: var(--font-mono); font-size: 10.5px; text-transform: uppercase; color: var(--map-bad-ink); }
  .fig { font-variant-numeric: tabular-nums; }
  .q { display: inline-block; min-width: 1.4em; padding: 0 4px; margin-left: 3px; text-align: center;
    border: 1px dashed var(--map-muted); border-radius: var(--radius); color: var(--map-muted);
    font-family: var(--font-mono); font-size: 11px; line-height: 15px; }
</style>
