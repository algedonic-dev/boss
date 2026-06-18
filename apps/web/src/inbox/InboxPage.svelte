<script lang="ts">
  // Inbox — port of apps/web/src/inbox/InboxPage.tsx.

  import PageHeader from '../ui/PageHeader.svelte';
  import { appNow } from '../shell/sim-clock.svelte';
  import FilterGroup from '../ui/FilterGroup.svelte';
  import FilterButton from '../ui/FilterButton.svelte';
  import SearchInput from '../ui/SearchInput.svelte';
  import type { Message, MessageKind } from './types';
  import type { Employee } from '../people/types';
  import { href, navigate } from '../router';
  import { session } from '../session/session.svelte';

  type KindFilter = MessageKind | 'all' | 'unread';

  let messages = $state<Message[]>([]);
  let employees = $state<Employee[]>([]);
  let kindFilter = $state<KindFilter>('all');
  let query = $state('');
  let composing = $state(false);

  let recipientId = $state('');
  let subject = $state('');
  let body = $state('');
  let sending = $state(false);

  let userId = $derived(
    session.value.kind === 'ready' ? session.value.user.id : '',
  );

  async function refreshInbox(): Promise<void> {
    if (!userId) return;
    try {
      const r = await fetch(`/api/messages/inbox/${encodeURIComponent(userId)}`);
      if (r.ok) {
        messages = (await r.json()) as Message[];
      }
    } catch {
      // empty
    }
  }

  $effect(() => {
    const uid = userId;
    if (!uid) return;
    void refreshInbox();
    // Load the roster alongside so the compose modal can offer names.
    (async () => {
      try {
        const r = await fetch('/api/people');
        if (r.ok) employees = (await r.json()) as Employee[];
      } catch {
        // ignore
      }
    })();
  });

  let employeeById = $derived.by(() => {
    const m = new Map<string, Employee>();
    for (const e of employees) m.set(e.id, e);
    return m;
  });

  let unread = $derived(messages.filter((m) => m.read_at === null));
  let directCount = $derived(messages.filter((m) => m.kind === 'direct').length);
  let signalCount = $derived(messages.filter((m) => m.kind === 'signal').length);

  let visible = $derived(
    messages.filter((m) => {
      if (kindFilter === 'unread' && m.read_at !== null) return false;
      if (kindFilter === 'direct' && m.kind !== 'direct') return false;
      if (kindFilter === 'signal' && m.kind !== 'signal') return false;
      if (query) {
        const q = query.toLowerCase();
        const hay = `${m.subject} ${m.body} ${m.sender_id}`.toLowerCase();
        if (!hay.includes(q)) return false;
      }
      return true;
    }),
  );

  async function markRead(m: Message): Promise<void> {
    if (m.read_at !== null) return;
    try {
      await fetch(`/api/messages/${encodeURIComponent(m.id)}/read`, {
        method: 'POST',
      });
      await refreshInbox();
    } catch {
      // ignore
    }
  }

  // Resolve the SPA path for a message's entity reference. The
  // message payload's `entity_path` is the canonical source of
  // truth — producers populate it directly. The legacy fallback
  // dispatch on `entity_type` survives for messages emitted
  // before producers started populating the field; it's
  // tenant-agnostic by design but every new tenant kind has to
  // teach the dispatcher unless its messages carry entity_path.
  function resolveEntityPath(
    ref: { entity_type: string; entity_id: string; entity_path?: string | null },
  ): string | null {
    if (ref.entity_path) return ref.entity_path;
    return entityPathFromType(ref.entity_type, ref.entity_id);
  }

  function entityPathFromType(type: string, id: string): string | null {
    switch (type) {
      // Generic across tenants.
      case 'job':
        return `/jobs/${encodeURIComponent(id)}`;
      case 'account':
        return `/accounts/${encodeURIComponent(id)}`;
      case 'vendor':
        return `/vendors/${encodeURIComponent(id)}`;
      case 'shipment':
        return `/shipping/${encodeURIComponent(id)}`;
      case 'employee':
        return `/people/${encodeURIComponent(id)}`;
      case 'invoice':
        return `/finance/invoices/${encodeURIComponent(id)}`;
      case 'part':
        return `/parts/${encodeURIComponent(id)}`;
      case 'opportunity':
        return `/sales/${encodeURIComponent(id)}`;
      // Used-device-shop tenant.
      case 'ticket':
        return `/service/${encodeURIComponent(id)}`;
      case 'device':
        return `/assets/${encodeURIComponent(id)}`;
      case 'refurb-job':
        return `/refurb/${encodeURIComponent(id)}`;
      default:
        return null;
    }
  }

  function formatAge(iso: string): string {
    const diff = appNow().getTime() - new Date(iso).getTime();
    const hours = Math.floor(diff / (1000 * 60 * 60));
    if (hours < 1) return 'just now';
    if (hours < 24) return `${hours}h`;
    const days = Math.floor(hours / 24);
    return `${days}d`;
  }

  function senderLabel(m: Message): string {
    if (m.sender_id === 'system') return 'System';
    return employeeById.get(m.sender_id)?.name ?? m.sender_id;
  }

  async function send(): Promise<void> {
    if (!recipientId || !subject || !body || !userId) return;
    sending = true;
    try {
      const r = await fetch('/api/messages/send', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          sender_id: userId,
          recipient_id: recipientId,
          subject,
          body,
        }),
      });
      if (r.ok) {
        composing = false;
        recipientId = '';
        subject = '';
        body = '';
        await refreshInbox();
      }
    } finally {
      sending = false;
    }
  }
</script>

<div class="catalog theme-exec">
  <PageHeader
    eyebrow="Inbox"
    title={`${messages.length} messages`}
    subtitle={`${unread.length} unread · ${signalCount} signals · ${directCount} direct`}
  />

  <div style="padding:0 32px 12px">
    <button class="hr-action-btn" onclick={() => (composing = true)}>Compose</button>
  </div>

  {#if composing}
    <div
      class="compose-overlay"
      role="presentation"
      onclick={() => (composing = false)}
    >
      <div
        class="compose-modal"
        role="dialog"
        aria-modal="true"
        tabindex="-1"
        onclick={(e) => e.stopPropagation()}
        onkeydown={(e) => e.stopPropagation()}
      >
        <div class="compose-header">
          <span class="compose-title">New Message</span>
          <button class="debug-close" onclick={() => (composing = false)}>✕</button>
        </div>
        <div class="compose-field">
          <label for="inbox-to">To</label>
          <select
            id="inbox-to"
            bind:value={recipientId}
            class="hr-select"
            style="width:100%"
          >
            <option value="">Select recipient...</option>
            {#each employees as e (e.id)}
              <option value={e.id}>{e.name} ({e.role})</option>
            {/each}
          </select>
        </div>
        <div class="compose-field">
          <label for="inbox-subject">Subject</label>
          <input
            id="inbox-subject"
            type="text"
            bind:value={subject}
            class="compose-input"
            placeholder="Subject..."
          />
        </div>
        <div class="compose-field">
          <label for="inbox-body">Message</label>
          <textarea
            id="inbox-body"
            bind:value={body}
            class="compose-textarea"
            rows="5"
            placeholder="Write your message..."
          ></textarea>
        </div>
        <div class="compose-actions">
          <button
            class="hr-action-btn"
            onclick={send}
            disabled={sending || !recipientId || !subject || !body}
          >
            {sending ? 'Sending...' : 'Send'}
          </button>
          <button class="hr-detail-btn" onclick={() => (composing = false)}>Cancel</button>
        </div>
      </div>
    </div>
  {/if}

  <div class="catalog-layout">
    <aside class="catalog-filters">
      <FilterGroup label="Search">
          <SearchInput bind:value={query} placeholder="Subject, sender…" />
      </FilterGroup>
      <FilterGroup label="Filter">
          <FilterButton active={kindFilter === 'all'} onclick={() => (kindFilter = 'all')}>
            All ({messages.length})
          </FilterButton>
          <FilterButton active={kindFilter === 'unread'} onclick={() => (kindFilter = 'unread')}>
            Unread ({unread.length})
          </FilterButton>
          <FilterButton active={kindFilter === 'direct'} onclick={() => (kindFilter = 'direct')}>
            Direct ({directCount})
          </FilterButton>
          <FilterButton active={kindFilter === 'signal'} onclick={() => (kindFilter = 'signal')}>
            Signals ({signalCount})
          </FilterButton>
      </FilterGroup>
    </aside>

    <section class="list-section">
      {#if visible.length === 0}
        <p class="empty">No messages match those filters.</p>
      {:else}
        <div class="inbox-list">
          {#each visible as m (m.id)}
            {@const isUnread = m.read_at === null}
            <div class="inbox-row {isUnread ? 'inbox-row-unread' : ''}">
              <div class="inbox-row-header">
                <span class="inbox-kind inbox-kind-{m.kind}">
                  {m.kind === 'signal' ? '⚡' : '✉'}
                </span>
                <span class="inbox-sender {isUnread ? 'inbox-sender-bold' : ''}">
                  {senderLabel(m)}
                </span>
                <span class="inbox-age">{formatAge(m.sent_at)}</span>
                {#if isUnread}
                  <button
                    class="inbox-mark-read"
                    onclick={() => markRead(m)}
                    title="Mark as read"
                  >
                    Mark read
                  </button>
                {/if}
              </div>
              <div class="inbox-subject {isUnread ? 'inbox-subject-bold' : ''}">
                {m.subject}
              </div>
              <div class="inbox-body">{m.body}</div>
              {#if m.entity_ref}
                {@const path = resolveEntityPath(m.entity_ref)}
                <div class="inbox-entity">
                  {#if path}
                    <a
                      href={href(path)}
                      class="inbox-entity-link"
                      onclick={(e) => {
                        e.preventDefault();
                        void markRead(m);
                        navigate(href(path));
                      }}
                    >
                      {m.entity_ref.entity_type}: {m.entity_ref.entity_id}
                    </a>
                  {:else}
                    <span class="mono">
                      {m.entity_ref.entity_type}: {m.entity_ref.entity_id}
                    </span>
                  {/if}
                </div>
              {/if}
            </div>
          {/each}
        </div>
      {/if}
    </section>
  </div>
</div>
