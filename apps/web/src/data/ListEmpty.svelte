<script lang="ts">
  // The line an empty list shows — read failed, nothing exists, or the
  // filters hid everything (backlog 0ef5e008). The words are decided in
  // readState.ts `emptyLine`, which is where the unit tests are; this only
  // puts them on the page, with the failure as an alert.
  import { emptyLine, type EmptyState, type EmptyWords } from './readState';

  let { view, words }: Readonly<{ view: EmptyState; words: EmptyWords }> = $props();

  let line = $derived(emptyLine(view, words));
</script>

{#if line?.alert}
  <p class="empty load-failed" role="alert">{line.text}</p>
{:else if line}
  <p class="empty">{line.text}</p>
{/if}
