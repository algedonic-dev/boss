<script lang="ts">
  // Inbox — port of apps/web/src/inbox/InboxPage.tsx.

  import ClassesReadFailed from '@boss/web-kit/ui/ClassesReadFailed.svelte';
  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import { appNow } from '@boss/web-kit/sim-clock';
  import FilterGroup from '@boss/web-kit/ui/FilterGroup.svelte';
  import FilterButton from '@boss/web-kit/ui/FilterButton.svelte';
  import SearchInput from '@boss/web-kit/ui/SearchInput.svelte';
  import WriteGate from '@boss/web-kit/ui/WriteGate.svelte';
  import type { InboxPage, Message } from './types';
  import { classLabel } from '../people/types';
  import { NAMES_URL, parseNames, recipientReason, type EmployeeName } from './roster';
  import { href, navigate } from '../router';
  import { session } from '@boss/web-kit/session/session.svelte';
  import { classesFor, loadClasses } from '@boss/web-kit/session/classes.svelte';
  import { fetchRemote, type Remote } from '../data/remote';
  import { countsOf, inboxPath, parseInbox, type KindFilter } from './read';
  import { emptyState, loadingRead, readStateOf } from '../data/readState';
  import ListEmpty from '../data/ListEmpty.svelte';
  import { postEach, postWrite, type BulkOutcome } from './writes';
  import { safeLinkHref } from '@boss/web-kit/links';

  /// `needs-you` is the default view, and the reason this file changed.
  ///
  /// The inbox opened on `all`: every message ever addressed to you, in
  /// one flat stream, thousands of them on a running deployment. The
  /// actionable items were in there — "I think I have actionable items
  /// somewhere in my Inbox, but it is unprocessable in its current
  /// form" — but finding them meant reading past every signal the
  /// machine had ever emitted.
  ///
  /// The distinction the data already carries: a `direct` message is a
  /// person (or an agent) addressing YOU and expecting something; a
  /// `signal` is the machine telling you a thing happened. Unread
  /// directs are the only category that is waiting on you, so that is
  /// what the page opens on. Everything else is one click away and
  /// nothing is hidden.
  ///
  /// Each view is a narrowing the SERVER answers, one page at a time
  /// (backlog 74da899d, page audit 5477d9eb GAP 3). The page read every
  /// row ever addressed to the viewer — 5,129 to one recipient in the
  /// audit window — rendered them in one list and filtered in the
  /// browser. Now it reads PAGE_SIZE rows of the view it shows, says
  /// which of how many those are, and pages; the header and the filter
  /// counts come from the read's per-kind counts of the whole inbox.

  /// The inbox itself, as a discriminated union — a failed fetch is a
  /// FAILED inbox, never an empty one. The old shape (`messages = []`
  /// plus a swallowed error) made an outage render "Nothing is
  /// waiting on you", which is a claim about the world the page had
  /// no basis for (packet 3fba9c35, the false-empty sweep).
  let inbox = $state<Remote<InboxPage>>({ kind: 'loading' });
  /// The read the inbox above answers. A view or page turned since is
  /// a read not yet answered, and the list says loading, not the rows
  /// of the view it left.
  let answered = $state('');
  let kindFilter = $state<KindFilter>('needs-you');
  const PAGE_SIZE = 100;
  /// The page belongs to the view it was turned under, so a filter
  /// change lands back on the first page without an effect of its own
  /// (JobsListPage's pager, d1310776).
  let turned = $state<{ filter: KindFilter; offset: number }>({ filter: 'needs-you', offset: 0 });
  let offset = $derived(turned.filter === kindFilter ? turned.offset : 0);
  let roster = $state<Remote<ReadonlyArray<EmployeeName>>>({ kind: 'loading' });
  let rosterViewer = $state('');
  let rosterAsked = 0;
  let query = $state('');
  let composing = $state(false);

  let recipientId = $state('');
  let subject = $state('');
  let body = $state('');
  let sending = $state(false);

  /// A write the server refused, said where it was asked (page audit
  /// 5477d9eb; ./writes.ts has the history). Mark read's answer rides
  /// on its row, keyed by message id; Send's rides in the modal, which
  /// stays open so nothing typed is lost.
  let markReadRefusals = $state<Readonly<Record<string, string>>>({});
  let sendRefusal = $state<string | null>(null);

  /// The page's OUT third (page audit 5477d9eb, GAP 8; backlog
  /// 5963a322). One Mark read at a time was the only way a message
  /// left view — used once in the audit window against 201 messages —
  /// while POST /api/messages/{id}/archive sat unused. Archive takes a
  /// row out of the inbox (the read leaves archived rows out, 8578b91e);
  /// the bulk bar applies Mark read to every shown unread row, or
  /// Archive to the checked ones, one per-row write each, and says how
  /// many landed. A refusal lands on its row, as Mark read's does.
  let archiveRefusals = $state<Readonly<Record<string, string>>>({});
  let selected = $state<ReadonlySet<string>>(new Set());
  let bulkBusy = $state(false);
  let bulkNote = $state<{ text: string; refused: boolean } | null>(null);

  let userId = $derived(
    session.value.kind === 'ready' ? session.value.user.id : '',
  );

  // A prior viewer's scoped identities cannot become this viewer's To
  // list while its read is pending (backlog 7d1c11a3).
  let rosterRead = $derived<Remote<ReadonlyArray<EmployeeName>>>(
    rosterViewer === userId ? roster : { kind: 'loading' },
  );
  let employees = $derived(rosterRead.kind === 'ready' ? rosterRead.data : []);
  let recipientBlock = $derived(recipientReason(rosterRead, recipientId));
  let sendBlock = $derived(recipientBlock ?? (!subject ? 'Enter a subject' : !body ? 'Enter a message' : null));

  async function refreshRoster(): Promise<void> {
    const viewer = userId;
    const mine = ++rosterAsked;
    rosterViewer = viewer;
    roster = { kind: 'loading' };
    if (!viewer) return;
    const out = await fetchRemote(NAMES_URL, parseNames);
    if (mine !== rosterAsked || viewer !== userId) return;
    roster = out;
  }

  /// The read of the view and page shown.
  let readPath = $derived(userId ? inboxPath(userId, kindFilter, offset, PAGE_SIZE) : '');

  /// Reads answer out of order when views change quickly; only the
  /// latest one asked for lands.
  let asked = 0;

  async function refreshInbox(): Promise<void> {
    const path = readPath;
    if (!path) return;
    const mine = ++asked;
    // Only the inbox envelope is an inbox: any other 200 is a failed
    // read, never an empty one (backlog e2679b23 (c); ./read.ts).
    const out = await fetchRemote(path, parseInbox(path));
    if (mine !== asked) return;
    inbox = out;
    answered = path;
  }

  $effect(() => {
    if (readPath) void refreshInbox();
  });

  /// The page of rows shown — only the read for the view and page the
  /// viewer is on; while that read is out, none.
  let messages = $derived(
    inbox.kind === 'ready' && answered === readPath ? inbox.data.data : [],
  );
  /// The whole inbox's numbers, from the last read that landed: they
  /// do not change with the view, so a view in flight keeps them.
  let counts = $derived(inbox.kind === 'ready' ? countsOf(inbox.data.kinds) : countsOf([]));
  /// How many rows the shown view holds in all, of which `messages` is
  /// one page.
  let viewTotal = $derived(inbox.kind === 'ready' && answered === readPath ? inbox.data.total : 0);

  /// A filter button's count, only from a read that landed: a failed or
  /// pending read has no count to give, and zeros would be the empty
  /// inbox's words (backlog e2679b23 (b)).
  function counted(n: number): string {
    return inbox.kind === 'ready' ? ` (${n})` : '';
  }

  /// The To list names roles the way the People pages do, from the
  /// Class registry, rather than printing the code (e2679b23 (e)).
  let roleClasses = $derived(classesFor('employee', 'role'));

  $effect(() => {
    const uid = userId;
    if (!uid) return;
    void loadClasses('employee');
    // The inbox read is its own effect, keyed on the view and page.
    // The scoped names projection needs none of the full profile/pay.
    void refreshRoster();
  });

  let employeeById = $derived.by(() => {
    const m = new Map<string, EmployeeName>();
    for (const e of employees) m.set(e.id, e);
    return m;
  });

  /// The view's own narrowing is the server's; the search narrows the
  /// page it answered, by what the row shows.
  let visible = $derived(
    messages.filter((m) => {
      if (query) {
        const q = query.toLowerCase();
        // The sender as the row SHOWS it, and its id: a search that
        // read only the id found nothing for the name on the screen
        // (backlog e2679b23 (a)).
        const hay = `${m.subject} ${m.body} ${senderLabel(m)} ${m.sender_id}`.toLowerCase();
        if (!hay.includes(q)) return false;
      }
      return true;
    }),
  );

  /// The pager says which rows of how many the list shows, and only
  /// when the view holds more than one page.
  let paged = $derived(
    inbox.kind === 'ready' && answered === readPath && (offset > 0 || offset + messages.length < viewTotal),
  );

  // Read failed, no messages, or the filters hid them (backlog 0ef5e008).
  // A view in flight is loading; an inbox with rows that the view or
  // the search leaves none of is the filters' doing, not an empty one.
  let listState = $derived(
    emptyState(
      [{
        source: readPath,
        state: inbox.kind === 'loading' || (inbox.kind === 'ready' && answered !== readPath)
          ? loadingRead
          : readStateOf(inbox),
      }],
      counts.all,
      visible.length,
    ),
  );

  /// What the bulk bar acts on is always what is SHOWN: a row checked
  /// under one filter and hidden by the next is not archived unseen.
  let visibleUnread = $derived(visible.filter((m) => m.read_at === null));
  let selectedShown = $derived(visible.filter((m) => selected.has(m.id)));
  let allShownSelected = $derived(
    visible.length > 0 && visible.every((m) => selected.has(m.id)),
  );

  /// True once the message is read. A refusal lands on the row and
  /// answers false; an admitted write clears any earlier refusal.
  async function markRead(m: Message): Promise<boolean> {
    if (m.read_at !== null) return true;
    const out = await postWrite(`/api/messages/${encodeURIComponent(m.id)}/read`);
    markReadRefusals = Object.fromEntries(
      Object.entries(markReadRefusals).filter(([id]) => id !== m.id),
    );
    if (out.kind === 'refused') {
      markReadRefusals = { ...markReadRefusals, [m.id]: out.reason };
      return false;
    }
    await refreshInbox();
    return true;
  }

  /// The entity link marks the message read, then goes. It AWAITS the
  /// write: fired unawaited, a refusal would answer on a page already
  /// left, which is the swallow this replaces (129da587). A refused
  /// write keeps the viewer here, where the row says why, and the row
  /// offers the same link without the write.
  ///
  /// A read-only guest's link is only the navigation: Mark read is a
  /// write the gate withholds everywhere else on the row, so the link
  /// must not send it either (backlog e2679b23 (d)).
  async function openEntity(m: Message, path: string): Promise<void> {
    if (session.readonly || (await markRead(m))) navigate(href(path));
  }

  /// A refusal map with `done` cleared and `refused` added.
  function settle(
    prior: Readonly<Record<string, string>>,
    out: BulkOutcome,
  ): Readonly<Record<string, string>> {
    const cleared = Object.fromEntries(
      Object.entries(prior).filter(([id]) => !out.done.includes(id)),
    );
    return { ...cleared, ...out.refused };
  }

  /// "Marked 2 of 3 read — 1 refused; each row says why."
  function noteFor(said: string, out: BulkOutcome): typeof bulkNote {
    const refused = Object.keys(out.refused).length;
    return {
      text: said + (refused ? ` — ${refused} refused; each row says why.` : '.'),
      refused: refused > 0,
    };
  }

  async function archive(m: Message): Promise<void> {
    const out = await postEach([m.id], (id) => `/api/messages/${encodeURIComponent(id)}/archive`);
    archiveRefusals = settle(archiveRefusals, out);
    if (out.done.length) await refreshInbox();
  }

  async function markAllRead(): Promise<void> {
    const ids = visibleUnread.map((m) => m.id);
    bulkBusy = true;
    bulkNote = null;
    const out = await postEach(ids, (id) => `/api/messages/${encodeURIComponent(id)}/read`);
    markReadRefusals = settle(markReadRefusals, out);
    bulkNote = noteFor(`Marked ${out.done.length} of ${ids.length} read`, out);
    bulkBusy = false;
    await refreshInbox();
  }

  async function archiveSelected(): Promise<void> {
    const ids = selectedShown.map((m) => m.id);
    bulkBusy = true;
    bulkNote = null;
    const out = await postEach(ids, (id) => `/api/messages/${encodeURIComponent(id)}/archive`);
    archiveRefusals = settle(archiveRefusals, out);
    selected = new Set([...selected].filter((id) => !out.done.includes(id)));
    bulkNote = noteFor(`Archived ${out.done.length} of ${ids.length}`, out);
    bulkBusy = false;
    await refreshInbox();
  }

  function toggleSelected(id: string, on: boolean): void {
    selected = on
      ? new Set([...selected, id])
      : new Set([...selected].filter((s) => s !== id));
  }

  function selectAllShown(on: boolean): void {
    const shown = new Set(visible.map((m) => m.id));
    selected = on
      ? new Set([...selected, ...shown])
      : new Set([...selected].filter((s) => !shown.has(s)));
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
    return employeeById.get(m.sender_id)?.name?.trim() || m.sender_id;
  }

  async function send(): Promise<void> {
    if (sending || sendBlock !== null) return;
    if (!recipientId || !subject || !body || !userId) return;
    sending = true;
    sendRefusal = null;
    const out = await postWrite('/api/messages/send', {
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        sender_id: userId,
        recipient_id: recipientId,
        subject,
        body,
      }),
    });
    sending = false;
    if (out.kind === 'refused') {
      sendRefusal = out.reason;
      return;
    }
    composing = false;
    recipientId = '';
    subject = '';
    body = '';
    await refreshInbox();
  }

  function openCompose(): void {
    sendRefusal = null;
    composing = true;
  }
</script>

<div class="catalog theme-exec">
  <!-- The headline is a claim about the world; the page only gets to
       make it from a loaded inbox. While loading or failed it says
       neither "nothing waiting" nor a count. -->
  <PageHeader
    eyebrow="Inbox"
    title={inbox.kind !== 'ready'
      ? 'Inbox'
      : counts.needsYou === 0
        ? 'Nothing is waiting on you'
        : `${counts.needsYou} waiting on you`}
    subtitle={inbox.kind === 'ready'
      ? `${counts.unread} unread · ${counts.direct} direct · ${counts.signal} signals · ${counts.all} total`
      : inbox.kind === 'failed'
        ? 'Your inbox could not be read.'
        : 'Loading…'}
  />
  <ClassesReadFailed subjectKind="employee" what="roles" fallback="Roles show by code, not by their registry names." />

  {#if rosterRead.kind === 'failed'}
    <p class="roster-note" role="status">Sender names unavailable — {rosterRead.error}. Sender ids are retained.</p>
  {/if}

  <div style="padding:0 32px 12px">
    <!-- The composer's entry stands behind the readonly gate: a guest
         sees Compose disabled with the sign-in note, not a live modal
         whose Send 403s. -->
    <WriteGate>
      <button class="btn btn-sm btn-primary" onclick={openCompose}>Compose</button>
    </WriteGate>
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
            disabled={rosterRead.kind !== 'ready' || !employees.length}
            class="hr-select"
            style="width:100%"
          >
            <option value="">Select recipient...</option>
            {#each employees as e (e.id)}
              <option value={e.id}>{e.name?.trim() || e.id} ({classLabel(e.role, roleClasses)})</option>
            {/each}
          </select>
        </div>
        {#if rosterRead.kind === 'loading'}
          <p role="status">Loading recipients…</p>
        {:else if rosterRead.kind === 'failed'}
          <p class="roster-refused" role="alert">Recipients unavailable — {rosterRead.error}</p>
          <button class="btn btn-sm" onclick={refreshRoster}>Retry recipients</button>
        {:else if !employees.length}
          <p role="status">No recipients are available in this read</p>
        {/if}
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
        {#if sendRefusal !== null}
          <p class="compose-refused" role="alert">Not sent — {sendRefusal}</p>
        {/if}
        <div class="compose-actions">
          <button
            class="btn btn-primary"
            onclick={send}
            disabled={sending || sendBlock !== null}
          >
            {sending ? 'Sending...' : 'Send'}
          </button>
          <button class="btn" onclick={() => (composing = false)}>Cancel</button>
        </div>
        {#if sendBlock !== null && !(rosterRead.kind === 'ready' && !employees.length)}
          <p class="send-unavailable" role="status">{sendBlock}</p>
        {/if}
      </div>
    </div>
  {/if}

  <div class="catalog-layout">
    <aside class="catalog-filters">
      <FilterGroup label="Search">
          <SearchInput bind:value={query} placeholder="Subject, sender…" label="Search messages" />
      </FilterGroup>
      <FilterGroup label="Filter">
          <!-- First and default. The other four are still here and
               nothing is hidden — this only decides what you land on. -->
          <FilterButton
            active={kindFilter === 'needs-you'}
            onclick={() => (kindFilter = 'needs-you')}
          >
            Waiting on you{counted(counts.needsYou)}
          </FilterButton>
          <FilterButton active={kindFilter === 'all'} onclick={() => (kindFilter = 'all')}>
            All{counted(counts.all)}
          </FilterButton>
          <FilterButton active={kindFilter === 'unread'} onclick={() => (kindFilter = 'unread')}>
            Unread{counted(counts.unread)}
          </FilterButton>
          <FilterButton active={kindFilter === 'direct'} onclick={() => (kindFilter = 'direct')}>
            Direct{counted(counts.direct)}
          </FilterButton>
          <FilterButton active={kindFilter === 'signal'} onclick={() => (kindFilter = 'signal')}>
            Signals{counted(counts.signal)}
          </FilterButton>
      </FilterGroup>
    </aside>

    <section class="list-section">
      <!-- Above the list, not in it: a bulk write that empties the
           filter still says what it did. -->
      {#if bulkNote !== null}
        <p
          class="inbox-bulk-note {bulkNote.refused ? 'inbox-bulk-note-refused' : ''}"
          role={bulkNote.refused ? 'alert' : 'status'}
        >
          {bulkNote.text}
        </p>
      {/if}
      {#if listState.kind !== 'rows'}
        <!-- A failed load is a failure, an empty inbox is not the
             filters' doing, and neither is "no match" (0ef5e008). -->
        <ListEmpty view={listState} words={{ what: 'your inbox', noun: 'messages' }} />
        {#if listState.kind === 'failed'}
          <div style="padding:0 32px">
            <button class="btn btn-sm" onclick={() => void refreshInbox()}>Retry</button>
          </div>
        {/if}
      {:else}
        <!-- The bulk bar acts on what is shown (backlog 5963a322). It
             and the rows stand behind ONE gate: each row's Mark read,
             Archive and checkbox is a write affordance too, and they
             sat outside it — a guest could press every one into a 403
             (backlog e2679b23 (d)). One gate, one note, not one per
             row; the links stay live, since a fieldset does not
             disable an anchor and a guest may read what they name. -->
        <WriteGate>
          <div class="inbox-bulk">
            <label class="inbox-bulk-all">
              <input
                type="checkbox"
                checked={allShownSelected}
                onchange={(e) => selectAllShown(e.currentTarget.checked)}
              />
              Select all shown
            </label>
            <button
              class="inbox-mark-read"
              onclick={() => void markAllRead()}
              disabled={bulkBusy || visibleUnread.length === 0}
            >
              Mark all read ({visibleUnread.length})
            </button>
            <button
              class="inbox-mark-read"
              onclick={() => void archiveSelected()}
              disabled={bulkBusy || selectedShown.length === 0}
            >
              Archive selected ({selectedShown.length})
            </button>
          </div>
          <div class="inbox-list">
            {#each visible as m (m.id)}
              {@const isUnread = m.read_at === null}
              <div class="inbox-row {isUnread ? 'inbox-row-unread' : ''}">
                <div class="inbox-row-header">
                  <input
                    type="checkbox"
                    class="inbox-select"
                    aria-label="Select {m.subject}"
                    checked={selected.has(m.id)}
                    onchange={(e) => toggleSelected(m.id, e.currentTarget.checked)}
                  />
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
                  <button
                    class="inbox-mark-read inbox-archive"
                    onclick={() => void archive(m)}
                    title="Archive: take it out of the inbox"
                  >
                    Archive
                  </button>
                </div>
                {#if markReadRefusals[m.id]}
                  <p class="inbox-write-refused" role="alert">
                    Not marked read — {markReadRefusals[m.id]}
                  </p>
                {/if}
                {#if archiveRefusals[m.id]}
                  <p class="inbox-write-refused" role="alert">
                    Not archived — {archiveRefusals[m.id]}
                  </p>
                {/if}
                <div class="inbox-subject {isUnread ? 'inbox-subject-bold' : ''}">
                  {m.subject}
                </div>
                <div class="inbox-body">{m.body}</div>
                <!-- `entity_path` is the producer-owned SPA link: every
                     emitter that attaches an entity_ref populates it, so
                     the inbox never has to know tenant route shapes. A
                     missing path renders as plain text, not a link. -->
                {#if m.entity_ref}
                  {@const path = m.entity_ref.entity_path ?? null}
                  <div class="inbox-entity">
                    {#if path}
                      <a
                        href={safeLinkHref(href(path))}
                        class="inbox-entity-link"
                        onclick={(e) => {
                          e.preventDefault();
                          void openEntity(m, path);
                        }}
                      >
                        {m.entity_ref.entity_type}: {m.entity_ref.entity_id}
                      </a>
                      {#if markReadRefusals[m.id]}
                        <a
                          href={safeLinkHref(href(path))}
                          class="inbox-entity-link inbox-open-anyway"
                          onclick={(e) => {
                            e.preventDefault();
                            navigate(href(path));
                          }}
                        >
                          Open without marking read
                        </a>
                      {/if}
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
        </WriteGate>
      {/if}
      {#if paged}
        <!-- Which rows of how many the view holds, and the way to the
             rest (backlog 74da899d). Outside the list's branches: a
             search that leaves none of this page still needs the way
             on. The search reads the page shown, and says so. -->
        <nav class="inbox-pager" aria-label="Pages of messages">
          <p>
            Showing {messages.length > 0
              ? `${(offset + 1).toLocaleString()}–${(offset + messages.length).toLocaleString()}`
              : 'none'} of {viewTotal.toLocaleString()}, newest first{query
              ? ' — the search reads this page only'
              : ''}
          </p>
          <button
            type="button"
            class="btn btn-sm"
            disabled={offset === 0}
            onclick={() => (turned = { filter: kindFilter, offset: Math.max(0, offset - PAGE_SIZE) })}
          >
            Previous
          </button>
          <button
            type="button"
            class="btn btn-sm"
            disabled={messages.length === 0 || offset + messages.length >= viewTotal}
            onclick={() => (turned = { filter: kindFilter, offset: offset + messages.length })}
          >
            Next
          </button>
        </nav>
      {/if}
    </section>
  </div>
</div>
