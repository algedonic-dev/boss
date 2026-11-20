<script lang="ts">
  // Port of apps/web/src/ui/EntityLink.tsx.
  // Philosophy (per CLAUDE.md memory "Every reference clickable +
  // name-over-ID"): every entity id is a link; friendly label beats
  // raw id when one is available; ID-shaped kinds keep mono styling.

  import Link from './Link.svelte';
  import { entityHref, hasPage, ID_IS_LABEL, type EntityKind } from './entity-href';

  let {
    kind,
    id,
    label = '',
    className = '',
    mono,
    title,
  } = $props<{
    kind: EntityKind;
    id: string;
    label?: string | null;
    className?: string;
    mono?: boolean;
    title?: string;
  }>();

  let effectiveLabel = $derived(label && label.length > 0 ? label : id);
  let useMono = $derived(mono ?? ID_IS_LABEL.has(kind));
  let classes = $derived(
    ['entity-link', className, useMono ? 'mono' : '']
      .filter(Boolean)
      .join(' '),
  );
  let tooltip = $derived(title ?? (effectiveLabel !== id ? id : undefined));
</script>

<!-- A kind with no page (entity-href.ts PAGELESS_KINDS, design 01fa5bdf)
     is drawn as text: a link would land on not-found. -->
{#if hasPage(kind)}
  <Link to={entityHref(kind, id)} className={classes}>
    <span title={tooltip}>{effectiveLabel}</span>
  </Link>
{:else}
  <span class={classes} data-pageless={kind} title={tooltip}>{effectiveLabel}</span>
{/if}
