<script lang="ts">
  // Simulator app root. Parses the current route on mount + on every
  // popstate (back/forward + the navigate() helper, which dispatches a
  // synthetic popstate), and renders the active page inside SimShell.
  import { onMount } from 'svelte';
  import SimShell from './shell/SimShell.svelte';
  import CockpitPage from './CockpitPage.svelte';
  import ControlsPanel from './ControlsPanel.svelte';
  import { parseRoute, type Route } from './router';

  let route = $state<Route>({ kind: 'cockpit' });

  function sync(): void {
    route = parseRoute(window.location.pathname);
  }

  onMount(() => {
    sync();
    window.addEventListener('popstate', sync);
    return () => window.removeEventListener('popstate', sync);
  });
</script>

<SimShell {route}>
  {#if route.kind === 'controls'}
    <ControlsPanel />
  {:else}
    <CockpitPage />
  {/if}
</SimShell>
