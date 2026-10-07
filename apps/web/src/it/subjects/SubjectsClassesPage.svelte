<script lang="ts">
  // /it/registry/subjects — Subjects & Classes: the model's vocabulary,
  // read-only. Left: the SubjectKind taxonomy (boss-subject-kinds). Right:
  // the selected kind's Class registry (boss-classes), grouped by the
  // Subject attribute each class set keys (role / department / type / …),
  // under a line naming the kind's module and how many active workflows
  // name it. Authoring is deliberately out of scope — this is the "what
  // vocabulary does the running model speak?" surface beside
  // /it/registry/dispatcher and /it/operate/audit.
  //
  // Each of the three reads owns its failure line (page audit 9f7ba57d):
  // the kinds read the page-wide alert, the classes read a line in the
  // detail pane naming the kind it failed for, the workflows read a line
  // saying the count is unknown. A failed read is never an empty one —
  // the classes read used to paint "No classes registered" under the
  // alert (backlog d145e41d).

  import { onMount } from 'svelte';
  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import Section from '@boss/web-kit/ui/Section.svelte';
  import { manifest, moduleEnabled } from '@boss/web-kit/session/manifest.svelte';
  import { departmentRoster } from '@boss/web-kit/session/departments.svelte';
  import { href } from '../../router';
  import type { Remote } from '../../data/remote';
  import {
    listSubjectKinds,
    listClasses,
    listWorkflows,
    buildKindTree,
    groupClassesByAttribute,
    kindModule,
    workflowCountsByKind,
    type SubjectKind,
    type ClassRow,
    type KindTreeNode,
  } from './subjects';

  const message = (e: unknown): string => (e instanceof Error ? e.message : String(e));

  let tree = $state<ReadonlyArray<KindTreeNode>>([]);
  let kindsByCode = $state<Map<string, SubjectKind>>(new Map());
  let selected = $state<string | null>(null);
  let classes = $state<Remote<ReadonlyArray<ClassRow>>>({ kind: 'loading' });
  let workflowCounts = $state<Remote<ReadonlyMap<string, number>>>({ kind: 'loading' });
  let loadingKinds = $state(true);
  let kindsError = $state<string | null>(null);

  async function loadKinds(): Promise<void> {
    loadingKinds = true;
    try {
      const all = await listSubjectKinds();
      tree = buildKindTree(all);
      kindsByCode = new Map(all.map((k) => [k.kind, k]));
      kindsError = null;
      const first = tree[0];
      if (first) await select(first.kind.kind);
    } catch (e) {
      kindsError = message(e);
    } finally {
      loadingKinds = false;
    }
  }

  async function loadWorkflowCounts(): Promise<void> {
    try {
      workflowCounts = { kind: 'ready', data: workflowCountsByKind(await listWorkflows()) };
    } catch (e) {
      workflowCounts = { kind: 'failed', error: message(e) };
    }
  }

  async function select(kind: string): Promise<void> {
    selected = kind;
    classes = { kind: 'loading' };
    let next: Remote<ReadonlyArray<ClassRow>>;
    try {
      next = { kind: 'ready', data: await listClasses(kind) };
    } catch (e) {
      next = { kind: 'failed', error: message(e) };
    }
    // A slower answer for a kind the operator has since clicked away from
    // must not paint over the kind now selected.
    if (selected === kind) classes = next;
  }

  onMount(() => {
    void loadKinds();
    void loadWorkflowCounts();
  });

  let selectedKind = $derived(selected ? (kindsByCode.get(selected) ?? null) : null);
  let grouped = $derived(classes.kind === 'ready' ? groupClassesByAttribute(classes.data) : []);
  let kindCount = $derived(kindsByCode.size);
  let selectedModule = $derived(selectedKind ? kindModule(selectedKind) : null);

  /** The kind whose ROWS live in a registry of their own (backlog
   *  c87e3d6d, decided on this page's audit 9f7ba57d): a department's
   *  Classes are only its four functions, and the departments are the
   *  `departments` table behind GET /api/departments — the chrome bar's
   *  roster, read once at boot. The panel says so, with the count, so
   *  the four function rows are never read as the company's four
   *  departments. */
  const REGISTRY_BACKED_KIND = 'department';
  let departmentCount = $derived(departmentRoster()?.length ?? null);

  /** "module off on this instance" is the manifest's word, not the row's
   *  (backlog 92ea2e00): the row says which module, the tenant manifest
   *  says whether this instance runs it. */
  const moduleOff = (m: string): boolean => manifest.value.kind === 'ready' && !moduleEnabled(m);

  function fmtVal(v: unknown): string {
    if (v === null || v === undefined) return '';
    if (typeof v === 'string' || typeof v === 'number' || typeof v === 'boolean') return String(v);
    return JSON.stringify(v);
  }
</script>

{#snippet kindCode(k: SubjectKind)}
  <!-- The module rides on the tree row too, so dormant vocabulary reads
       as dormant at a glance rather than one click at a time (92ea2e00). -->
  {@const m = kindModule(k)}
  <span class="sc-kind-code mono"
    >{k.kind}{#if m}<span class="sc-kind-module" class:off={moduleOff(m)}
        > · {m}{moduleOff(m) ? ' (off)' : ''}</span
      >{/if}</span
  >
{/snippet}

{#snippet metaCell(meta: Readonly<Record<string, unknown>>)}
  {@const entries = Object.entries(meta)}
  {#if entries.length === 0}
    <span class="sc-dim">—</span>
  {:else}
    <span class="sc-chips">
      {#each entries as [k, v] (k)}
        <span class="sc-chip"><span class="sc-chip-k">{k}</span>{fmtVal(v)}</span>
      {/each}
    </span>
  {/if}
{/snippet}

<div class="subjects theme-exec">
  <PageHeader
    eyebrow="Platform · Model vocabulary"
    title="Subjects & Classes"
    subtitle={loadingKinds
      ? 'Loading…'
      : kindsError && kindCount === 0
        ? // A failed kinds read leaves the map empty, and "0 subject
          // kinds" read as an empty registry (sweep c3e4edcc).
          'Subject-kind count unknown — the registry read failed'
        : `${kindCount} subject kind${kindCount === 1 ? '' : 's'} · the Class registry`}
  />

  {#if kindsError}
    <!-- The shared failure marker (sweep c3e4edcc); no inline colour or
         padding, which would outrank its troubled ink and rail card. -->
    <p class="empty load-failed" role="alert" style="margin:0 24px">Failed to load: {kindsError}</p>
  {/if}

  <div class="sc-body">
    <aside class="sc-tree">
      <div class="sc-tree-head">Subject kinds</div>
      {#each tree as node (node.kind.kind)}
        <button
          class="sc-kind sc-root"
          class:active={selected === node.kind.kind}
          onclick={() => select(node.kind.kind)}
        >
          <span class="sc-kind-label">{node.kind.label}</span>
          {@render kindCode(node.kind)}
        </button>
        {#each node.children as child (child.kind)}
          <button
            class="sc-kind sc-child"
            class:active={selected === child.kind}
            onclick={() => select(child.kind)}
          >
            <span class="sc-kind-label">{child.label}</span>
            {@render kindCode(child)}
          </button>
        {/each}
      {/each}
    </aside>

    <div class="sc-detail">
      {#if selectedKind}
        <div class="sc-detail-head">
          <h2>
            {selectedKind.label}
            <span class="sc-detail-code mono">{selectedKind.kind}</span>
          </h2>
          {#if selectedKind.description}
            <p class="sc-detail-desc">{selectedKind.description}</p>
          {/if}
          <div class="sc-meta">
            {#if selectedKind.parent_kind}
              <span>parent <span class="mono">{selectedKind.parent_kind}</span></span>
            {/if}
            <!-- Was `owner {owning_team}`, which read `platform` on 24 of 24
                 kinds and so distinguished nothing (backlog 92ea2e00). The
                 row names its module; the manifest says whether this
                 instance runs it. -->
            {#if selectedModule}
              <span
                >module <span class="mono">{selectedModule}</span
                >{#if moduleOff(selectedModule)}<span class="sc-off"> · off on this instance</span
                  >{/if}</span
              >
            {:else}
              <span>platform kind</span>
            {/if}
            {#if workflowCounts.kind === 'ready'}
              {@const n = workflowCounts.data.get(selectedKind.kind) ?? 0}
              <span
                >{n === 0
                  ? 'no active workflow names this kind'
                  : `${n} active workflow${n === 1 ? ' names' : 's name'} this kind`}</span
              >
            {/if}
            {#if selectedKind.kind === REGISTRY_BACKED_KIND}
              <!-- A null roster is a read that has not answered, or failed:
                   never "0 departments". -->
              <span
                >{departmentCount === null
                  ? 'department count unknown — the departments registry has not answered'
                  : `${departmentCount} department${departmentCount === 1 ? '' : 's'} in the departments registry`}
                · <a href={href('/it')}>Department Map</a></span
              >
            {/if}
          </div>
          {#if workflowCounts.kind === 'failed'}
            <p class="empty load-failed sc-read-failed">
              Couldn't count the workflows naming {selectedKind.kind} — {workflowCounts.error}
            </p>
          {/if}
        </div>

        {#if classes.kind === 'loading'}
          <p class="empty">Loading classes…</p>
        {:else if classes.kind === 'failed'}
          <p class="empty load-failed" role="alert">
            Couldn't load the classes of {selectedKind.kind} — {classes.error}
          </p>
        {:else if grouped.length === 0}
          <p class="empty">
            No classes registered for <span class="mono">{selectedKind.kind}</span>. Classes are
            tenant reference data — see
            <code class="mono">docs/design/class-registry.md</code>.
          </p>
        {:else}
          {#each grouped as [attr, rows] (attr)}
            <Section title={`${attr} · ${rows.length}`} wide>
              <table class="data-table data-table-striped">
                <thead>
                  <tr>
                    <th>Code</th>
                    <th>Display name</th>
                    <th>Parent</th>
                    <th>Metadata</th>
                  </tr>
                </thead>
                <tbody>
                  {#each rows as c (c.code)}
                    <tr>
                      <td><span class="mono">{c.code}</span></td>
                      <td>{c.display_name}</td>
                      <td>
                        {#if c.parent_code}
                          <span class="mono">{c.parent_code}</span>
                        {:else}
                          <span class="sc-dim">—</span>
                        {/if}
                      </td>
                      <td>{@render metaCell(c.metadata)}</td>
                    </tr>
                  {/each}
                </tbody>
              </table>
            </Section>
          {/each}
        {/if}
      {:else if !loadingKinds && !kindsError}
        <!-- A failed kinds read has nothing to select: the alert above
             is the whole of what this page knows (backlog d145e41d). -->
        <p class="empty">Select a subject kind to see its classes.</p>
      {/if}
    </div>
  </div>
</div>

<style>
  .subjects {
    padding-bottom: 40px;
  }
  .sc-body {
    display: grid;
    grid-template-columns: 260px 1fr;
    gap: 24px;
    padding: 0 24px;
    align-items: start;
  }
  .sc-tree {
    position: sticky;
    top: 16px;
    border: 1px solid var(--hairline);
    border-radius: 8px;
    overflow: hidden;
    background: var(--ink);
  }
  .sc-tree-head {
    font-size: 11px;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: var(--static);
    padding: 10px 12px;
    background: var(--ink-raised);
    border-bottom: 1px solid var(--hairline);
  }
  .sc-kind {
    display: flex;
    flex-direction: column;
    gap: 1px;
    width: 100%;
    text-align: left;
    padding: 8px 12px;
    background: none;
    border: none;
    border-bottom: 1px solid var(--hairline);
    cursor: pointer;
  }
  .sc-kind:hover {
    background: var(--ink-raised);
  }
  .sc-kind.active {
    background: var(--signal-wash);
    box-shadow: inset 3px 0 0 var(--signal);
  }
  .sc-root .sc-kind-label {
    font-weight: 600;
  }
  .sc-child {
    padding-left: 24px;
  }
  .sc-kind-label {
    font-size: 13px;
    color: var(--fog);
  }
  .sc-kind-code {
    font-size: 11px;
    color: var(--static);
  }
  .sc-detail-head {
    padding: 4px 0 12px;
  }
  .sc-detail-head h2 {
    margin: 0;
    font-size: 18px;
  }
  .sc-detail-code {
    font-size: 13px;
    color: var(--static);
    font-weight: 400;
  }
  .sc-detail-desc {
    margin: 6px 0 0;
    color: var(--static);
    font-size: 13px;
    max-width: 60ch;
  }
  .sc-meta {
    display: flex;
    gap: 16px;
    margin-top: 6px;
    font-size: 12px;
    color: var(--static);
  }
  .sc-dim {
    color: var(--text-faint);
  }
  .sc-kind-module.off,
  .sc-off {
    color: var(--text-faint);
    font-style: italic;
  }
  /* Layout only: .load-failed draws the card, rail and ink. */
  .sc-read-failed {
    margin: 8px 0 0;
    font-size: 12px;
  }
  .sc-chips {
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
  }
  .sc-chip {
    font-size: 11px;
    background: var(--ink-raised);
    border-radius: 4px;
    padding: 1px 6px;
    color: var(--static);
  }
  .sc-chip-k {
    color: var(--static);
    margin-right: 4px;
  }
</style>
