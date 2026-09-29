<script lang="ts">
  // THE ONE LINE a surface draws when its Class registry read failed
  // (backlog 8d58d250, 2026-09-28). `classesFor` answers [] on error as
  // it does while loading, so a reader that renders only its rows cannot
  // tell an outage from an empty taxonomy — the two marketing-asset pages
  // grew this line first (e520c794), inline; twelve other readers stayed
  // silent. One component, so the wording cannot fork: "Couldn't load the
  // <what> — <url>: HTTP n. <fallback>", wearing `load-failed`, the one
  // failure marker the outage crawl asserts on (tests/mocked/_routes.ts).
  //
  // `subjectKind` is the kind the surface loads; `what` names the Classes
  // it reads in the reader's words ("roles", "account tiers"); `fallback`
  // says what the surface shows instead ("Kinds show as codes."). `style`
  // is layout only, for a line that sits in tight chrome (the sidebar).
  import { classesLoad } from '../session/classes.svelte';

  let { subjectKind, what, fallback, style = undefined } = $props<{
    subjectKind: string;
    what: string;
    fallback: string;
    style?: string;
  }>();

  let load = $derived(classesLoad(subjectKind));
</script>

{#if load.kind === 'error'}
  <p class="load-failed" role="alert" {style}>
    Couldn't load the {what} — {load.error}. {fallback}
  </p>
{/if}
