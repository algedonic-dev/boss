<script lang="ts">
  // Sign-in / sign-out control for the top perspective bar. Probes
  // /api/auth/me to decide which to show — the gateway 401s anonymous
  // demo sessions, so a demo visitor sees "Sign in" and a real
  // operator sees "Sign out". Styled for the dark bar. Render it once,
  // inside the perspective tab bar.
  let isLoggedIn = $state<boolean>(false);
  $effect(() => {
    (async () => {
      try {
        const r = await fetch('/api/auth/me');
        isLoggedIn = r.ok;
      } catch {
        isLoggedIn = false;
      }
    })();
  });

  async function signOut(): Promise<void> {
    try {
      await fetch('/api/auth/logout', { method: 'POST' });
    } catch {
      // Best-effort — redirect regardless; the next request re-mints
      // a demo session if logout didn't land.
    }
    window.location.href = '/login';
  }
</script>

{#if isLoggedIn}
  <button class="signin-btn" onclick={signOut}>Sign out</button>
{:else}
  <a class="signin-btn" href="/login">Sign in</a>
{/if}

<style>
  .signin-btn {
    background: transparent;
    border: 1px solid #44403c;
    border-radius: 6px;
    padding: 4px 11px;
    font-size: 12px;
    font-weight: 500;
    color: #e7e5e4;
    text-decoration: none;
    cursor: pointer;
    font-family: inherit;
    line-height: 1.4;
    white-space: nowrap;
  }
  .signin-btn:hover {
    background: #292524;
    color: #fff;
    border-color: #57534e;
  }
</style>
