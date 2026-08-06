<script lang="ts">
  // Feedback, from the chrome bar, on every surface.
  //
  // Submitting opens a `user-feedback` Job. That is the whole design:
  // feedback is work, so it goes in the Job model rather than a table
  // with its own screen. It inherits an owner, a policy-gated triage
  // step, an audit trail of who handled it and when, and a place in
  // the queues operators already read — none of which a bespoke
  // feedback store would have without rebuilding them.
  //
  // The Job's Subject is the surface the feedback is ABOUT: a `custom`
  // Subject whose id is the current route path. So "what have people
  // said about this page" is a Subject-history question, answerable by
  // the same machinery as everything else.
  //
  // No new endpoint — this posts to /api/jobs like any other Job.
  import { session } from './session/session.svelte';

  let open = $state(false);
  let message = $state('');
  let sending = $state(false);
  let sent = $state(false);
  let error = $state<string | null>(null);
  let box: HTMLTextAreaElement | null = $state(null);

  /// The surface being commented on. Read at submit time rather than
  /// on mount — the panel can outlive a client-side navigation.
  function currentRoute(): string {
    return typeof window === 'undefined' ? '/' : window.location.pathname;
  }

  function toggle(): void {
    open = !open;
    if (open) {
      sent = false;
      error = null;
      // Focus after the panel exists.
      queueMicrotask(() => box?.focus());
    }
  }

  async function submit(): Promise<void> {
    const body = message.trim();
    if (body.length === 0 || sending) return;
    sending = true;
    error = null;
    const route = currentRoute();
    const who = session.value.kind === 'ready' ? session.value.user.id : 'anonymous';
    try {
      const r = await fetch('/api/jobs', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          kind: 'user-feedback',
          // Identity-first Subject. The route path IS the subject id,
          // the same shape design-doc-review uses for a doc path.
          subject: { subject_kind: 'custom', id: route },
          title: `Feedback on ${route}`,
          owner_id: who,
          priority: 'standard',
          status: 'open',
          metadata: { message: body, route, submitted_by: who },
          tags: ['feedback'],
        }),
      });
      if (!r.ok) throw new Error(`HTTP ${r.status}: ${await r.text()}`);
      sent = true;
      message = '';
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      sending = false;
    }
  }

  function onKey(e: KeyboardEvent): void {
    if (e.key === 'Escape') open = false;
    // Cmd/Ctrl+Enter submits — the convention for a textarea whose
    // Enter key means newline.
    if ((e.metaKey || e.ctrlKey) && e.key === 'Enter') void submit();
  }
</script>

<div class="fb">
  <button
    class="fb-trigger"
    type="button"
    aria-expanded={open}
    aria-haspopup="dialog"
    onclick={toggle}
    title="Send feedback about this page"
  >
    Feedback
  </button>

  {#if open}
    <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
    <div class="fb-panel" role="dialog" aria-label="Send feedback" onkeydown={onKey}>
      {#if sent}
        <p class="fb-done">
          Thanks — that opened a feedback Job for <code>{currentRoute()}</code>. It is
          queued for triage like any other work.
        </p>
        <button class="fb-send" type="button" onclick={() => (open = false)}>Close</button>
      {:else}
        <label class="fb-label" for="fb-message">
          Feedback on <code>{currentRoute()}</code>
        </label>
        <textarea
          id="fb-message"
          bind:this={box}
          bind:value={message}
          rows="4"
          placeholder="What is wrong, missing, or confusing here?"
        ></textarea>
        {#if error}<p class="fb-err">{error}</p>{/if}
        <div class="fb-actions">
          <span class="fb-hint">⌘/Ctrl + Enter</span>
          <button
            class="fb-send"
            type="button"
            disabled={sending || message.trim().length === 0}
            onclick={submit}
          >
            {sending ? 'Sending…' : 'Send'}
          </button>
        </div>
      {/if}
    </div>
  {/if}
</div>

<style>
  .fb {
    position: relative;
    display: flex;
    align-items: center;
  }
  /* The chrome bar is an unconditionally dark surface (#0c0a09) that
     sets no `color`, so anything inside it must declare its own. This
     button used `color: inherit` and therefore picked up the DOCUMENT
     text colour: fine in dark theme, near-black on near-black in
     light theme. Reported as "I can't read this feedback button" —
     and correctly diagnosed as a light/dark issue.

     Matching `.signin-btn` rather than inventing another treatment:
     the two are the only bordered buttons in the bar, and they sat
     one gap apart with different borders, radii, and padding. */
  .fb-trigger {
    background: transparent;
    border: 1px solid #44403c;
    border-radius: 6px;
    padding: 4px 11px;
    font-family: inherit;
    font-size: 12px;
    font-weight: 500;
    line-height: 1.4;
    color: #e7e5e4;
    cursor: pointer;
    white-space: nowrap;
  }
  .fb-trigger:hover {
    background: #292524;
    color: #fff;
    border-color: #57534e;
  }
  .fb-trigger:focus-visible,
  .fb-send:focus-visible {
    outline: 2px solid currentColor;
    outline-offset: 2px;
  }
  /* Anchored below the 44px bar, right-aligned so it never runs off
     the viewport on the control that sits nearest the edge. */
  .fb-panel {
    position: absolute;
    top: calc(100% + 8px);
    right: 0;
    z-index: 60;
    width: 320px;
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 12px;
    border-radius: 8px;
    background: var(--card, #fff);
    color: var(--text, #1c1917);
    border: 1px solid var(--border, #e7e5e4);
    box-shadow: 0 8px 24px rgba(0, 0, 0, 0.18);
  }
  .fb-label {
    font-size: 12px;
    color: var(--text-dim, #78716c);
  }
  .fb-panel code {
    font-size: 11px;
    background: var(--bg, #f5f5f4);
    padding: 1px 4px;
    border-radius: 3px;
  }
  .fb-panel textarea {
    font: inherit;
    font-size: 13px;
    padding: 6px;
    border: 1px solid var(--border, #e7e5e4);
    border-radius: 4px;
    resize: vertical;
  }
  .fb-actions {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .fb-hint {
    font-size: 11px;
    color: var(--text-dim, #a8a29e);
    margin-right: auto;
  }
  .fb-send {
    font: inherit;
    font-size: 12px;
    padding: 4px 12px;
    border-radius: 4px;
    border: 1px solid var(--border, #e7e5e4);
    background: var(--bg, #f5f5f4);
    color: inherit;
    cursor: pointer;
  }
  .fb-send:disabled {
    opacity: 0.5;
    cursor: default;
  }
  .fb-done {
    font-size: 13px;
    margin: 0;
  }
  .fb-err {
    font-size: 12px;
    color: #b91c1c;
    margin: 0;
  }
</style>
