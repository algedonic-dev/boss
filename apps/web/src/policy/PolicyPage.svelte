<script lang="ts">
  // /it/registry/policy — the role × resource × action view of the policy
  // rule table, and the door to edit one rule's scope. What it may draw is
  // decided in ./policyView (page audit b2af346a, 2026-09-27) and pinned
  // by tests/mocked/policy-page.mocked.spec.ts.

  import ClassesReadFailed from '@boss/web-kit/ui/ClassesReadFailed.svelte';
  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import Section from '@boss/web-kit/ui/Section.svelte';
  import EditPolicyFlyout from './EditPolicyFlyout.svelte';
  import { session } from '@boss/web-kit/session/session.svelte';
  import { classesFor, loadClasses } from '@boss/web-kit/session/classes.svelte';
  import { failedWithReason, failedRead, loadingRead, okRead, type ReadState } from '../data/readState';
  import {
    defaultRole, matrixAxes, policySubtitle, roleList, rulesOf, scopeForDisplay,
  } from './policyView';
  import type { PolicyRule } from './policyTypes';

  let rules = $state<ReadonlyArray<PolicyRule>>([]);
  // A read that has not answered is `loading`, and the matrix waits for it
  // (backlog 5a7ab4b1): the page used to open on '0 active rules' and a
  // grid of dashes, a pending read drawn as a zero.
  let read = $state<ReadState>(loadingRead);
  // The viewer's pick; until they make one the page shows defaultRole.
  let picked = $state<string | null>(null);
  let editing = $state<PolicyRule | null>(null);

  async function load(): Promise<void> {
    read = loadingRead;
    try {
      const r = await fetch('/api/policy/rules');
      // The service's reason rides the refusal (F5 of the review behind
      // backlog 5763e52e): a bare status sent the reader to re-derive
      // what the policy service had already said.
      if (!r.ok) {
        read = failedWithReason(r.status, await r.text());
        return;
      }
      rules = (await r.json()) as PolicyRule[];
      read = okRead;
    } catch (e) {
      read = failedRead(e instanceof Error ? e.message : String(e));
    }
  }

  $effect(() => {
    void load();
    void loadClasses('employee');
  });

  let roles = $derived(roleList(rules, classesFor('employee', 'role').map((c) => c.code)));
  let sessionRole = $derived(session.value.kind === 'ready' ? session.value.user.role : null);
  let role = $derived(picked ?? defaultRole(roles, rules, sessionRole));
  let axes = $derived(matrixAxes(rules));
  let held = $derived(role === null ? [] : rulesOf(rules, role));
  let byCell = $derived(new Map(held.map((r) => [`${r.resource}\u0000${r.action}`, r])));

  let currentUserId = $derived(
    session.value.kind === 'ready' ? session.value.user.id : '',
  );
</script>

<div class="catalog theme-exec">
  <PageHeader
    eyebrow="Platform · Policy"
    title="Policy rules"
    subtitle={policySubtitle(read, rules, roles)}
  />

  {#if read.kind === 'failed'}
    <!-- The shared failure marker (sweep c3e4edcc). -->
    <p class="empty load-failed" role="alert" style="margin:0 24px">Failed to load rules: {read.error}</p>
  {/if}

  <div style="padding:0 24px 16px; display:flex; gap:12px; align-items:center">
    <label style="font-size:13px">
      Role:&nbsp;
      <select
        value={role ?? ''}
        onchange={(e) => (picked = e.currentTarget.value)}
        style="padding:4px 8px; font-size:13px"
      >
        {#each roles as r (r)}
          <option value={r}>{r}</option>
        {/each}
      </select>
    </label>
    <button type="button" class="btn" onclick={load} disabled={read.kind === 'loading'}>
      {read.kind === 'loading' ? 'Loading…' : 'Refresh'}
    </button>
  </div>

  <ClassesReadFailed
    subjectKind="employee"
    what="roles"
    fallback="Roles holding no rules are not listed."
    style="margin:0 24px 16px"
  />

  <!-- The matrix is drawn only from a read that landed. Under a failure it
       is withheld (backlog 5358ad72): dashes under the alert read as "this
       role holds nothing", and a previous read's grants under "count
       unknown" read as current. -->
  {#if read.kind === 'ok'}
    {#if role === null}
      <p class="empty" style="margin:0 24px">No roles: the policy table holds no rules and the Class registry lists no role.</p>
    {:else if held.length === 0}
      <!-- An honest answer of nothing, readable as that (backlog 47b7f120). -->
      <p class="empty" style="margin:0 24px">{role} holds no rules in this table.</p>
    {:else}
      <Section title={`${role} — resource × action matrix`} wide>
        <table class="data-table data-table-striped">
          <thead>
            <tr>
              <th>Resource</th>
              {#each axes.actions as a (a)}
                <th>{a}</th>
              {/each}
            </tr>
          </thead>
          <tbody>
            {#each axes.resources as resource (resource)}
              <tr>
                <td class="mono">{resource}</td>
                {#each axes.actions as action (action)}
                  {@const cell = byCell.get(`${resource}\u0000${action}`)}
                  <td>
                    {#if cell}
                      <button
                        type="button"
                        class="btn"
                        style="padding:2px 8px; font-size:12px"
                        onclick={() => (editing = cell)}
                      >
                        {scopeForDisplay(cell.scope)}
                      </button>
                    {:else}
                      <span style="color:var(--static); font-size:12px">—</span>
                    {/if}
                  </td>
                {/each}
              </tr>
            {/each}
          </tbody>
        </table>
      </Section>
    {/if}
  {/if}

  {#if editing}
    <EditPolicyFlyout
      rule={editing}
      changedBy={currentUserId}
      onClose={() => (editing = null)}
      onSaved={() => {
        editing = null;
        void load();
      }}
    />
  {/if}
</div>
