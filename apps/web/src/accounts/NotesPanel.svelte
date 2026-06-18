<script lang="ts">
  // Notes / interactions compose + list. Optimistic prepend on post.

  import Section from '../ui/Section.svelte';
  import { appNow } from '../shell/sim-clock.svelte';
  import EntityLink from '../ui/EntityLink.svelte';
  import { createAccountNote } from './api';
  import type { AccountNote } from './types';
  import { session } from '../session/session.svelte';

  let { accountId, notes } = $props<{
    accountId: string;
    notes: ReadonlyArray<AccountNote>;
  }>();

  type Kind = 'note' | 'call' | 'meeting' | 'email' | 'interaction';

  let empNames = $state<Map<string, string>>(new Map());
  let appended = $state<AccountNote[]>([]);
  let draft = $state('');
  let kind = $state<Kind>('note');
  let posting = $state(false);
  let error = $state<string | null>(null);

  $effect(() => {
    (async () => {
      try {
        const r = await fetch('/api/people');
        if (!r.ok) return;
        const roster = (await r.json()) as Array<{ id: string; name: string }>;
        const m = new Map<string, string>();
        for (const e of roster) m.set(e.id, e.name);
        empNames = m;
      } catch {
        // Ignore.
      }
    })();
  });

  let merged = $derived([...appended, ...notes]);
  let canPost = $derived(draft.trim().length > 0 && !posting);

  let actorId = $derived(
    session.value.kind === 'ready' ? session.value.user.id : '',
  );

  async function post(): Promise<void> {
    if (!canPost) return;
    posting = true;
    error = null;
    try {
      const created = await createAccountNote({
        account_id: accountId,
        actor_id: actorId,
        body: draft.trim(),
        kind,
      });
      appended = [created, ...appended];
      draft = '';
    } catch (e) {
      error = String(e);
    } finally {
      posting = false;
    }
  }

  function daysAgo(iso: string): string {
    const then = new Date(iso).getTime();
    const now = appNow().getTime();
    const d = Math.floor((now - then) / 86_400_000);
    if (d < 1) return 'today';
    if (d === 1) return '1d';
    if (d < 30) return `${d}d`;
    if (d < 365) return `${Math.floor(d / 30)}mo`;
    return `${Math.floor(d / 365)}y`;
  }
</script>

<Section title={`Notes & interactions (${merged.length})`}>
    <div class="pp-note-compose">
      <textarea
        class="pp-note-textarea"
        rows="2"
        placeholder="Add a note, call summary, or interaction…"
        bind:value={draft}
      ></textarea>
      <div class="pp-note-compose-row">
        <select
          class="pp-note-kind"
          bind:value={kind}
          disabled={posting}
        >
          <option value="note">Note</option>
          <option value="call">Call</option>
          <option value="meeting">Meeting</option>
          <option value="email">Email</option>
          <option value="interaction">Interaction</option>
        </select>
        <button
          class="pp-note-post"
          onclick={post}
          disabled={!canPost}
        >
          {posting ? 'Posting…' : 'Add note'}
        </button>
      </div>
      {#if error}<div class="pp-note-error">{error}</div>{/if}
    </div>

    {#if merged.length === 0}
      <p class="empty">No notes logged.</p>
    {:else}
      <ul class="pp-note-list">
        {#each merged.slice(0, 10) as n (n.id)}
          <li class="pp-note-item">
            <div class="pp-note-item-header">
              <span class="pp-note-kind-chip">{n.kind}</span>
              <span class="pp-note-item-author">
                <EntityLink
                  kind="employee"
                  id={n.actor_id}
                  label={empNames.get(n.actor_id)}
                  mono={false}
                />
              </span>
              <span class="pp-note-item-when">{daysAgo(n.created_at)} ago</span>
            </div>
            <div class="pp-note-item-body">{n.body}</div>
          </li>
        {/each}
      </ul>
    {/if}
</Section>
