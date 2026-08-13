<script lang="ts">
  // The job-packet card (David's call, 2026-08-12; feedback d69033dd):
  // the one visual for a packet anywhere a queue renders. Protocol
  // names the rail color, tags ride as chips, and a simulated packet
  // wears a dashed border + SIM chip so test traffic can never pass
  // for real work. Promoted here from the train yard so every queue
  // lens draws the same card. Pure presentation plus one affordance —
  // every fact comes off the card data, and double-click (or Enter
  // when focused) opens the packet's job detail.
  import { navigate } from '../nav';
  import { entityHref } from './entity-href';
  import { protocolHue, type PacketCardData } from './packet-card';

  type Props = Readonly<{ card: PacketCardData; size?: 'dock' | 'consist' }>;
  let { card, size = 'dock' }: Props = $props();

  const hue = $derived(protocolHue(card.kind));
  const shownTags = $derived(
    card.tags.filter(t => !['sim', 'simulated', 'synthetic'].includes(t.toLowerCase())).slice(0, 3),
  );

  function open(): void {
    navigate(entityHref('job', card.id));
  }
  function onKeydown(e: KeyboardEvent): void {
    if (e.key === 'Enter') {
      e.preventDefault();
      open();
    }
  }
</script>

<!-- A generic div, not <article>: the card is interactive (role=link
     + tabindex), and a11y rules bar interactive roles on sectioning
     elements. -->
<div
  class="packet"
  class:compact={size === 'consist'}
  class:sim={card.sim}
  style="--pk: {hue}"
  title="{card.title} — double-click to open the job"
  role="link"
  tabindex="0"
  ondblclick={open}
  onkeydown={onKeydown}
>
  <div class="pk-head">
    <span class="pk-kind">{card.kind}</span>
    {#if card.sim}<span class="pk-sim">SIM</span>{/if}
  </div>
  <div class="pk-title">{card.title}</div>
  <div class="pk-foot">
    <span class="pk-branch">{card.branch}</span>
    {#each shownTags as t (t)}<span class="pk-tag">{t}</span>{/each}
  </div>
  {#if card.skipReason}
    <div class="pk-skip">LEFT BEHIND — {card.skipReason}</div>
  {/if}
</div>

<style>
  .packet {
    background: var(--card, var(--ink, #12161c));
    border: 1px solid var(--hairline, #2a3138);
    border-left: 3px solid var(--pk);
    padding: 8px 12px;
    min-width: 0;
    cursor: pointer;
    transition: border-color 120ms ease;
  }
  /* The navigation affordance: the hairline takes the packet's own
     hue on hover; keyboard focus gets the same accent as an outline. */
  .packet:hover {
    border-color: var(--pk);
  }
  .packet:focus-visible {
    outline: 1px solid var(--pk);
    outline-offset: 2px;
  }
  .packet.sim {
    border-style: dashed;
    border-left-style: solid;
  }
  .pk-head {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .pk-kind {
    font-family: var(--font-mono, ui-monospace, monospace);
    font-size: 10px;
    letter-spacing: var(--ls-nav, 0.14em);
    text-transform: uppercase;
    color: var(--pk);
  }
  .pk-sim {
    font-family: var(--font-mono, ui-monospace, monospace);
    font-size: 10px;
    letter-spacing: 0.14em;
    color: var(--static, #7a838c);
    border: 1px dashed var(--static, #7a838c);
    padding: 0 5px;
  }
  .pk-title {
    font-size: 13.5px;
    margin: 3px 0 4px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .pk-foot {
    display: flex;
    align-items: center;
    gap: 6px;
    min-width: 0;
  }
  .pk-branch {
    font-family: var(--font-mono, ui-monospace, monospace);
    font-size: 11.5px;
    color: var(--static, #7a838c);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .pk-tag {
    font-family: var(--font-mono, ui-monospace, monospace);
    font-size: 10px;
    letter-spacing: 0.08em;
    color: var(--static, #7a838c);
    border: 1px solid var(--hairline, #2a3138);
    padding: 0 5px;
    flex: none;
  }
  .pk-skip {
    font-family: var(--font-mono, ui-monospace, monospace);
    font-size: 10.5px;
    color: var(--warn, #d9a441);
    margin-top: 4px;
    letter-spacing: 0.05em;
  }
  .packet.compact {
    padding: 5px 9px;
  }
  .packet.compact .pk-title {
    font-size: 12px;
    margin: 2px 0 2px;
    max-width: 260px;
  }
  .packet.compact .pk-foot {
    display: none;
  }
</style>
