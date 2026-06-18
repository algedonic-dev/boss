<script lang="ts">
  // Port of apps/web/src/ui/EntityLink.tsx.
  // Philosophy (per CLAUDE.md memory "Every reference clickable +
  // name-over-ID"): every entity id is a link; friendly label beats
  // raw id when one is available; ID-shaped kinds keep mono styling.

  import Link from './Link.svelte';
  import { entityHref, ID_IS_LABEL, type EntityKind } from './entity-href';

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

<Link to={entityHref(kind, id)} className={classes}>
  <span title={tooltip}>{effectiveLabel}</span>
</Link>
