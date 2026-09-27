<script lang="ts">
  // Home → Views. The personal rung of the extensibility ladder.
  //
  // Below "author a Workflow" there used to be nothing: an operator who
  // wanted to look at the information a different way could ask for a
  // frontend change or keep a spreadsheet. This is the surface that
  // ends that, and the spreadsheet is what it is competing with — so
  // making a View has to cost less than opening one.
  //
  // A View holds a query and a layout, never rows. Its content comes
  // from the same projections every other surface reads, which is why
  // two people running the same View see the same numbers.
  import FilterButton from '@boss/web-kit/ui/FilterButton.svelte';
  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import Section from '@boss/web-kit/ui/Section.svelte';
  import Link from '@boss/web-kit/ui/Link.svelte';
  import { readPeopleRow, session, type ViewerRead } from '@boss/web-kit/session/session.svelte';
  import {
    SOURCE_FIELDS,
    type View,
    type ViewInput,
    type ViewLayout,
    type ViewResults,
    type ViewSource,
    type Visibility,
  } from './types';
  import {
    NO_VIEWER_LINE,
    cellHref,
    hasNoViewer,
    refusedLine,
    scannedRows,
    sendWrite,
    sharerName,
    shownLine,
  } from './present';

  let views = $state<ReadonlyArray<View>>([]);
  let loading = $state(true);
  let error = $state<string | null>(null);

  // Results are keyed by view id rather than held on a "selected"
  // view: running two Views and comparing them is the obvious next
  // thing an operator does, and a single slot would forbid it.
  let results = $state<Record<string, ViewResults | undefined>>({});
  let readAt = $state<Record<string, Date | undefined>>({});
  let running = $state<Record<string, boolean>>({});
  let rowErrors = $state<Record<string, string | undefined>>({});
  // A refused Delete, on the row it was refused for (backlog 0148c461).
  // It used to write the page's `error`, whose branch replaced every
  // View with a bare "HTTP 403" — a refused write read as a failed list.
  let deleteErrors = $state<Record<string, string | undefined>>({});
  // Who shared each View not the viewer's own, read from their people
  // row (backlog 00ab7ddd) — "shared by emp-002" named nobody.
  let sharers = $state<Record<string, ViewerRead | undefined>>({});

  // Draft state for the composer. `editingId` names the View the draft
  // was loaded from by Edit (backlog c4f3c53a): the server has had
  // `PUT /api/views/{id}` since the page was written and nothing called
  // it, so fixing a filter typo meant Delete and retype.
  let editingId = $state<string | null>(null);
  let draftTitle = $state('');
  let draftSource = $state<ViewSource>('jobs');
  let draftFilter = $state('');
  let draftColumns = $state<string[]>([]);
  let draftLayout = $state<ViewLayout>('table');
  let draftVisibility = $state<Visibility>('private');
  let saving = $state(false);
  let saveError = $state<string | null>(null);

  /// Mirrors EVENT_PUSHABLE / STEP_PUSHABLE in query.rs.
  const PUSHABLE_BY_SOURCE: Readonly<Record<string, string>> = {
    events: 'kind, source, subject_kind, subject_id, or a timestamp range',
    steps: 'status, kind or assignee_id',
    jobs: 'kind, status, owner_id, subject_kind, subject_id, priority, or a created_at range',
    subjects: 'kind, id, label, or a created_at range',
  };

  let viewerId = $derived(session.value.kind === 'ready' ? session.value.user.id : '');
  let availableFields = $derived(SOURCE_FIELDS[draftSource]);

  async function load(): Promise<void> {
    if (!viewerId) return;
    loading = true;
    error = null;
    try {
      // No viewer_id: identity travels in the request the
      // gateway stamps. Sending it as a param let any caller list
      // any user's private Views by naming them.
      const r = await fetch('/api/views');
      if (!r.ok) throw new Error(`views: HTTP ${r.status}`);
      views = (await r.json()) as View[];
      void nameSharers(views);
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      loading = false;
    }
  }

  /// One people-row read per sharer not yet read. A failed read is kept
  /// and said beside the id (sharerName), never dropped.
  async function nameSharers(list: ReadonlyArray<View>): Promise<void> {
    const owners = [...new Set(list.map((v) => v.owner_id))].filter(
      (id) => id !== viewerId && !(id in sharers),
    );
    await Promise.all(
      owners.map(async (id) => {
        const read = await readPeopleRow(id);
        sharers = { ...sharers, [id]: read };
      }),
    );
  }

  async function run(v: View): Promise<void> {
    running = { ...running, [v.id]: true };
    rowErrors = { ...rowErrors, [v.id]: undefined };
    try {
      const r = await fetch(`/api/views/${v.id}/results?limit=100`);
      if (!r.ok) throw new Error(`HTTP ${r.status}: ${await r.text()}`);
      results = { ...results, [v.id]: (await r.json()) as ViewResults };
      readAt = { ...readAt, [v.id]: new Date() };
    } catch (e) {
      rowErrors = { ...rowErrors, [v.id]: e instanceof Error ? e.message : String(e) };
    } finally {
      running = { ...running, [v.id]: false };
    }
  }

  function resetDraft(): void {
    editingId = null;
    draftTitle = '';
    draftFilter = '';
    draftColumns = [];
    saveError = null;
  }

  function startEdit(v: View): void {
    editingId = v.id;
    draftTitle = v.title;
    draftSource = v.source;
    draftFilter = v.filter;
    draftColumns = [...v.columns];
    draftLayout = v.layout;
    draftVisibility = v.visibility;
    saveError = null;
  }

  /// Save the draft: a new View, or — after Edit — a replacement of the
  /// View it was loaded from, through the same body.
  async function save(): Promise<void> {
    saving = true;
    saveError = null;
    const input: ViewInput = {
      // owner_id is not on the wire — the server takes it from the
      // authenticated caller.
      title: draftTitle.trim(),
      source: draftSource,
      filter: draftFilter.trim(),
      columns: draftColumns,
      layout: draftLayout,
      visibility: draftVisibility,
    };
    const edited = editingId;
    // 422 carries the filter parse error. Showing it against the form
    // is the whole reason the API distinguishes it from a 500 — the
    // author can fix a typo without guessing.
    const out = await sendWrite(edited ? `/api/views/${edited}` : '/api/views', {
      method: edited ? 'PUT' : 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(input),
    });
    saving = false;
    if (out.kind === 'refused') {
      saveError = refusedLine('save', out.reason);
      return;
    }
    // A replaced View's old result answered its old query.
    if (edited) results = { ...results, [edited]: undefined };
    resetDraft();
    await load();
  }

  async function remove(v: View): Promise<void> {
    deleteErrors = { ...deleteErrors, [v.id]: undefined };
    const out = await sendWrite(`/api/views/${v.id}`, { method: 'DELETE' });
    if (out.kind === 'refused') {
      deleteErrors = { ...deleteErrors, [v.id]: refusedLine('delete', out.reason) };
      return;
    }
    if (editingId === v.id) resetDraft();
    await load();
  }

  function toggleColumn(f: string): void {
    draftColumns = draftColumns.includes(f)
      ? draftColumns.filter((c) => c !== f)
      : [...draftColumns, f];
  }

  /// Column order for a result table: what the View asked for, or the
  /// keys of the first row when it asked for everything.
  function columnsOf(v: View, res: ViewResults): ReadonlyArray<string> {
    if (v.columns.length > 0) return v.columns;
    return res.rows.length > 0 ? Object.keys(res.rows[0]!) : [];
  }

  function cell(value: unknown): string {
    if (value === null || value === undefined) return '—';
    if (typeof value === 'object') return JSON.stringify(value);
    return String(value);
  }

  // One trigger, not two. `onMount(load)` plus an effect that also
  // called load() fired twice on any mount where the session was
  // already resolved, racing two identical requests. The effect alone
  // covers both cases: it runs once on mount and again if the session
  // resolves later.
  let loadedFor = $state<string | null>(null);
  $effect(() => {
    const id = viewerId;
    if (id && loadedFor !== id) {
      loadedFor = id;
      void load();
    }
  });
</script>

<PageHeader
  title="Views"
  subtitle="Your own compositions over the information layer — a query and a layout, never a copy of the data."
/>

<!-- Snippets rather than inline markup so the table and the list lay a
     cell out the same way: an id that names a packet, step or subject
     is a link to it (backlog 00ab7ddd). -->
{#snippet cellOf(v: View, row: Readonly<Record<string, unknown>>, c: string)}
  {@const to = cellHref(v.source, c, row)}
  {#if to}<Link {to}>{cell(row[c])}</Link>{:else}{cell(row[c])}{/if}
{/snippet}

{#if session.readonly}
  <!-- A read-only session is offered no write (backlog 14ef5e06), as
       MePage and InboxPage offer none: every Save and Delete it made
       was refused. Display only — the server's refusal stays the
       enforcement. -->
  <p class="v-msg">
    This session is read-only: it can run the Views shared with it, but not save or
    change one.
  </p>
{:else}
<Section title={editingId ? 'Edit view' : 'New view'} wide>
  <div class="v-form">
    <label class="v-field">
      <span>Title</span>
      <!-- A platform kind every tenant ships, so the example runs on the
           instance showing it (backlog 6d4eea00); it named a demo
           tenant's wholesale-keg-order, with 0 packets here. -->
      <input bind:value={draftTitle} placeholder="Open backlog items" />
    </label>

    <label class="v-field">
      <span>Source</span>
      <select bind:value={draftSource}>
        <option value="jobs">Jobs — the work</option>
        <option value="steps">Steps — what the work is made of</option>
        <option value="subjects">Subjects — the things work is about</option>
        <option value="events">Events — what happened</option>
      </select>
    </label>

    <label class="v-field v-field-wide">
      <span>Filter <em>optional</em></span>
      <input
        class="mono"
        bind:value={draftFilter}
        placeholder={'status = "open" AND kind = "backlog-item"'}
      />
    </label>

    <div class="v-field v-field-wide">
      <span>Columns <em>none selected shows everything</em></span>
      <div class="v-chips">
        {#each availableFields as f (f)}
          <FilterButton active={draftColumns.includes(f)} onclick={() => toggleColumn(f)}>
            {f}
          </FilterButton>
        {/each}
      </div>
    </div>

    <label class="v-field">
      <span>Layout</span>
      <select bind:value={draftLayout}>
        <option value="table">Table</option>
        <option value="list">List</option>
        <option value="count">Count</option>
      </select>
    </label>

    <label class="v-field">
      <span>Visibility</span>
      <select bind:value={draftVisibility}>
        <option value="private">Private — only me</option>
        <option value="shared">Shared — anyone can open it</option>
      </select>
    </label>

    <div class="v-actions">
      <button
        class="btn"
        type="button"
        disabled={saving || draftTitle.trim().length === 0 || !viewerId}
        onclick={save}
      >
        {saving ? 'Saving…' : editingId ? 'Save changes' : 'Save view'}
      </button>
      {#if editingId}
        <button class="v-del v-cancel" type="button" onclick={resetDraft}>Cancel edit</button>
      {/if}
      {#if saveError}
        <!-- A refused Save wears the shared failure marker, as the
             page's two reads do (backlog 0148c461). -->
        <p class="v-msg load-failed" role="alert">{saveError}</p>
      {/if}
    </div>
  </div>
</Section>
{/if}

{#if session.value.kind === 'loading' || (viewerId && loading)}
  <p class="v-msg">Loading views…</p>
{:else if hasNoViewer(session.value.kind)}
  <!-- `loading` started true and load() waits for a viewer, so a
       session that resolved to nobody used to read "Loading views…"
       for good (backlog 04754a40). -->
  <p class="v-msg">{NO_VIEWER_LINE}</p>
{:else if error}
  <!-- The shared failure marker (sweep c3e4edcc), here and on a view
       whose run failed. Only the LIST read writes `error`; a write's
       refusal is said against the write. -->
  <p class="v-msg v-list-failed load-failed" role="alert">{error}</p>
{:else if views.length === 0}
  <p class="v-msg">
    No views yet. A view is a saved question — pick a source, describe what you
    want to see, and it stays answerable.
  </p>
{:else}
  {#each views as v (v.id)}
    {@const res = results[v.id]}
    {@const at = readAt[v.id]}
    <div class="v-view" data-view={v.id}>
    <Section title={v.title} wide>
      <div class="v-meta">
        <span class="v-tag">{v.source}</span>
        <span class="v-tag">{v.layout}</span>
        <span class="v-tag" class:v-tag-shared={v.visibility === 'shared'}>
          {v.visibility}
        </span>
        {#if v.owner_id !== viewerId}
          <span class="v-tag">shared by {sharerName(v.owner_id, sharers[v.owner_id])}</span>
        {/if}
        {#if v.filter}
          <code class="v-filter">{v.filter}</code>
        {/if}
        <span class="v-spacer"></span>
        <button class="btn" type="button" onclick={() => run(v)} disabled={running[v.id]}>
          {running[v.id] ? 'Running…' : 'Run'}
        </button>
        {#if v.owner_id === viewerId && !session.readonly}
          <button class="v-del v-edit" type="button" onclick={() => startEdit(v)}>Edit</button>
          <button class="v-del" type="button" onclick={() => remove(v)}>Delete</button>
        {/if}
      </div>

      {#if deleteErrors[v.id]}
        <p class="v-msg load-failed" role="alert">{deleteErrors[v.id]}</p>
      {/if}
      {#if rowErrors[v.id]}
        <p class="v-msg load-failed" role="alert">{rowErrors[v.id]}</p>
      {:else if res}
        <p class="v-count">
          {res.matched}
          {res.matched === 1 ? 'match' : 'matches'}
          {#if at}
            <!-- What the page shows of what matched, and when it was
                 read (backlog 22d5bfcd): limit=100 rows under a count
                 that runs to the scan ceiling, on a result that stays
                 up until the next Run. -->
            <span class="v-shown">{shownLine(res, at)}</span>
          {/if}
          {#if res.truncated && res.pushed_down === 0 && v.filter}
            <!-- The weak case: nothing in the filter could be answered
                 by the database, so this counted only the newest rows.
                 A match older than that window is simply absent, and
                 "0 matches" here does not mean none exist. -->
            <strong class="v-trunc">
              — this filter could not be narrowed in the database, so only the
              {scannedRows(v.source)} were examined. Older matches are
              not counted. Filtering on {PUSHABLE_BY_SOURCE[v.source] ?? 'a different field'} narrows it.
            </strong>
          {:else if res.truncated}
            <strong class="v-trunc">
              — more than this matched; the count is a floor, not a total
            </strong>
          {/if}
        </p>

        {#if v.layout !== 'count' && res.rows.length > 0}
          {#if v.layout === 'table'}
            <div class="v-scroll">
              <table class="v-table">
                <thead>
                  <tr>
                    {#each columnsOf(v, res) as c (c)}<th>{c}</th>{/each}
                  </tr>
                </thead>
                <tbody>
                  {#each res.rows as row, i (i)}
                    <tr>
                      {#each columnsOf(v, res) as c (c)}<td>{@render cellOf(v, row, c)}</td>{/each}
                    </tr>
                  {/each}
                </tbody>
              </table>
            </div>
          {:else}
            <ul class="v-list">
              {#each res.rows as row, i (i)}
                <li>
                  {#each columnsOf(v, res) as c, j (c)}{#if j > 0}{' · '}{/if}{@render cellOf(v, row, c)}{/each}
                </li>
              {/each}
            </ul>
          {/if}
        {/if}
      {/if}
    </Section>
    </div>
  {/each}
{/if}

<style>
  .v-form {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 12px 16px;
    max-width: 900px;
  }
  .v-field {
    display: flex;
    flex-direction: column;
    gap: 4px;
    font-size: 12px;
    color: var(--text-dim);
  }
  .v-field-wide {
    grid-column: 1 / -1;
  }
  .v-field em {
    font-style: normal;
    color: var(--static);
  }
  .mono {
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
  }
  .v-chips {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }
  .v-actions {
    grid-column: 1 / -1;
    display: flex;
    align-items: center;
    gap: 12px;
  }
  .v-meta {
    display: flex;
    align-items: center;
    gap: 8px;
    flex-wrap: wrap;
    margin-bottom: 10px;
  }
  .v-spacer {
    flex: 1 1 auto;
  }
  .v-tag {
    font-size: 11px;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--text-dim);
    border: 1px solid var(--border);
    border-radius: 4px;
    padding: 1px 6px;
  }
  .v-tag-shared {
    border-color: var(--clear);
    color: var(--ok);
  }
  .v-filter {
    font-size: 12px;
    background: var(--bg);
    padding: 2px 6px;
    border-radius: 4px;
  }
  .v-del {
    background: none;
    border: none;
    color: var(--err);
    font: inherit;
    font-size: 12px;
    cursor: pointer;
  }
  .v-count {
    font-size: 13px;
    margin: 0 0 8px;
  }
  .v-trunc {
    color: var(--warn);
    font-weight: 600;
  }
  .v-scroll {
    overflow-x: auto;
  }
  .v-table {
    border-collapse: collapse;
    font-size: 13px;
    width: 100%;
  }
  .v-table th,
  .v-table td {
    border-bottom: 1px solid var(--border);
    padding: 5px 10px;
    text-align: left;
    white-space: nowrap;
  }
  .v-table th {
    font-size: 11px;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--text-dim);
  }
  .v-list {
    margin: 0;
    padding-left: 18px;
    font-size: 13px;
  }
  .v-msg {
    color: var(--text-dim);
    font-size: 14px;
  }
  .v-edit,
  .v-cancel {
    color: var(--text-dim);
  }
  .v-shown {
    margin-left: 8px;
    font-size: 12px;
    color: var(--text-dim);
  }
</style>
