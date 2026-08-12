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
  /* Ghost button, §04: square corners, hairline border, mono caps.
     Hover inverts rather than tinting — the spec's one hover rule for
     buttons, and it keeps SIGNAL free for state that means something. */
  .signin-btn {
    background: transparent;
    border: 1px solid var(--hairline, #2a3138);
    border-radius: var(--radius, 0);
    padding: 5px 12px;
    font-family: var(--font-mono, ui-monospace, monospace);
    font-size: 11px;
    font-weight: 500;
    text-transform: uppercase;
    letter-spacing: var(--ls-nav, 0.14em);
    color: var(--fog, #e8ecef);
    text-decoration: none;
    cursor: pointer;
    line-height: 1.4;
    white-space: nowrap;
    transition: background 0.1s, color 0.1s, border-color 0.1s;
  }
  .signin-btn:hover {
    background: var(--fog, #e8ecef);
    color: var(--void, #0d1014);
    border-color: var(--fog, #e8ecef);
  }
</style>
