<script lang="ts">
  // One architecture diagram, drawn in the browser from its Mermaid
  // source (backlog 4718d918; decided on page audit 8cd38edd, gap 1).
  // The committed SVG renders this replaced drifted from their source
  // because nothing could regenerate them; a render made at view time
  // has no copy to drift. Mermaid loads by URL, on this page only — never
  // through the bundle, which the dev-server would inline into every
  // page's script (src/mermaid-vendor.ts has the measurement). The URL
  // is built at run time so no bundler tries to resolve it.
  //
  // A source Mermaid cannot parse is SHOWN, with the parser's message
  // and the source text: a blank box would read as "no diagram" rather
  // than "a broken one".
  import type { Mermaid } from 'mermaid';
  import type { KbDiagram } from './kbDiagrams';
  import { MERMAID_ENTRY, MERMAID_ROUTE } from './mermaidUrl';

  type Props = Readonly<{ diagram: KbDiagram }>;
  let { diagram }: Props = $props();

  type View =
    | { kind: 'loading' }
    | { kind: 'ready'; svg: string }
    | { kind: 'failed'; message: string };
  let view: View = $state({ kind: 'loading' });

  async function render(d: KbDiagram): Promise<View> {
    try {
      const url = new URL(`${MERMAID_ROUTE}${MERMAID_ENTRY}`, window.location.origin).href;
      const { default: mermaid } = (await import(/* by URL: see above */ url)) as { default: Mermaid };
      // `strict` (Mermaid's default, said out loud): label HTML is
      // sanitised and click handlers are off. The source is a repo file,
      // but what the page injects should not depend on that.
      mermaid.initialize({ startOnLoad: false, securityLevel: 'strict' });
      const { svg } = await mermaid.render(`kb-${d.file.replace(/\W/g, '-')}`, d.source);
      return { kind: 'ready', svg };
    } catch (e) {
      return { kind: 'failed', message: e instanceof Error ? e.message : String(e) };
    }
  }

  $effect(() => {
    let live = true;
    view = { kind: 'loading' };
    void render(diagram).then((next) => {
      if (live) view = next;
    });
    return () => {
      live = false;
    };
  });
</script>

<style>
  .arch-figure {
    margin: 0;
  }
  .arch-diagram {
    background: var(--ink);
    border: 1px solid var(--hairline);
    border-radius: 8px;
    padding: 16px;
    overflow: auto;
    max-height: 75vh;
  }
  /* At least 1600px wide and scrolled inside the box, as the committed
     renders were drawn (width: max(100%, 1600px)), so a wide diagram's
     labels stay readable; Mermaid's own inline max-width keeps a small
     diagram at its natural size rather than stretching it. */
  .arch-diagram > :global(svg) {
    display: block;
    margin: 0 auto;
    width: max(100%, 1600px);
    height: auto;
  }
  .arch-note {
    font-size: 13px;
    color: var(--static);
  }
  .arch-failed pre {
    white-space: pre-wrap;
    font-size: 12px;
  }
  .arch-caption {
    font-size: 12px;
    color: var(--static);
    margin-top: 6px;
    text-align: right;
  }
</style>

<figure class="arch-figure">
  <div class="arch-diagram" aria-busy={view.kind === 'loading'}>
    {#if view.kind === 'ready'}
      {@html view.svg}
    {:else if view.kind === 'loading'}
      <p class="arch-note">Drawing {diagram.label.toLowerCase()} from its source…</p>
    {:else}
      <div class="arch-failed">
        <p class="arch-note"><strong>This diagram's source did not render.</strong> Mermaid said: {view.message}</p>
        <pre>{diagram.source}</pre>
      </div>
    {/if}
  </div>
  <figcaption class="arch-caption">
    {diagram.label}, drawn from <code>docs/architecture/{diagram.file}</code> — the one file to edit to change it
  </figcaption>
</figure>
