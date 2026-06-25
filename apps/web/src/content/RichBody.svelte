<script lang="ts">
  // Render a plain-text body with entity-ID shortcodes promoted to
  // EntityLinks. Svelte port of apps/web/src/content/RichBody.tsx.

  import EntityLink from '@boss/web-kit/ui/EntityLink.svelte';
  import { tokenize } from './richBody';

  type Props = {
    body: string;
    employeeNames?: ReadonlyMap<string, string>;
    className?: string;
  };
  let { body, employeeNames, className = '' }: Props = $props();

  let tokens = $derived(tokenize(body));
</script>

<span class={className} style="white-space:pre-wrap">
  {#each tokens as t, i (i)}
    {#if t.kind === 'text'}
      {t.text}
    {:else}
      {@const label =
        t.entityKind === 'employee' ? employeeNames?.get(t.id) : undefined}
      <EntityLink kind={t.entityKind} id={t.id} label={label ?? undefined} />
    {/if}
  {/each}
</span>
