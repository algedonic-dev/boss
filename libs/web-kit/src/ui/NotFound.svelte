<script lang="ts">
  // Detail-page not-found (and kin: bad-payload, gone) state —
  // the eyebrow/title/back-link shape detail pages hand-roll as a
  // bare exec-header when the subject they were asked for doesn't
  // exist. Children render as the explanatory line under the header.
  import Breadcrumb from './Breadcrumb.svelte';
  import PageHeader from './PageHeader.svelte';

  let { eyebrow, title, backHref, backLabel, children } = $props<{
    eyebrow?: string;
    title: string;
    /// Route for the "← back to the list" breadcrumb; omit to
    /// render no breadcrumb.
    backHref?: string;
    backLabel?: string;
    children?: () => any;
  }>();
</script>

{#if backHref}
  <Breadcrumb to={backHref}>← {backLabel ?? 'Back'}</Breadcrumb>
{/if}
<PageHeader {eyebrow} {title} />
{#if children}
  <p class="empty">{@render children()}</p>
{/if}
