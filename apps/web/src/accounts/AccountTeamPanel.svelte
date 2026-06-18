<script lang="ts">
  // Account team panel — assign/unassign roles per account.
  //
  // Optimistic local overrides layered on the server-provided
  // `team` array so the UI reflects changes before the next fetch.
  // Matches apps/web/src/accounts/AccountPage.tsx AccountTeamPanel.

  import Section from '../ui/Section.svelte';
  import EntityLink from '../ui/EntityLink.svelte';
  import {
    assignAccountTeamMember,
    unassignAccountTeamMember,
  } from './api';
  import {
    ACCOUNT_TEAM_ROLES,
    ACCOUNT_TEAM_ROLE_LABEL,
    type AccountTeamMember,
    type AccountTeamRole,
  } from './types';
  import { session } from '../session/session.svelte';
  import { appNow } from '../shell/sim-clock.svelte';

  let { accountId, team } = $props<{
    accountId: string;
    team: ReadonlyArray<AccountTeamMember>;
  }>();

  let empNames = $state<Map<string, string>>(new Map());
  let employees = $state<Array<{ id: string; name: string }>>([]);

  $effect(() => {
    (async () => {
      try {
        const r = await fetch('/api/people');
        if (!r.ok) return;
        const roster = (await r.json()) as Array<{ id: string; name: string }>;
        const m = new Map<string, string>();
        for (const e of roster) m.set(e.id, e.name);
        empNames = m;
        employees = roster;
      } catch {
        // Ignore — panel still renders without friendly names.
      }
    })();
  });

  let overrides = $state<AccountTeamMember[]>([]);
  let removed = $state<Set<string>>(new Set());
  let showAdd = $state(false);
  let pickRole = $state<AccountTeamRole>('customer-success');
  let pickEmp = $state('');
  let busy = $state(false);
  let error = $state<string | null>(null);

  let merged = $derived.by(() => {
    const byRole = new Map<string, AccountTeamMember>();
    for (const m of team) {
      if (!removed.has(m.role)) byRole.set(m.role, m);
    }
    for (const m of overrides) byRole.set(m.role, m);
    return Array.from(byRole.values());
  });

  let rolesInUse = $derived(new Set(merged.map((m) => m.role)));
  let availableRoles = $derived(
    ACCOUNT_TEAM_ROLES.filter((r) => !rolesInUse.has(r)),
  );

  let actorId = $derived(
    session.value.kind === 'ready' ? session.value.user.id : '',
  );

  async function add(): Promise<void> {
    if (!pickEmp || busy) return;
    busy = true;
    error = null;
    try {
      await assignAccountTeamMember({
        account_id: accountId,
        employee_id: pickEmp,
        role: pickRole,
        actor_id: actorId,
      });
      const nowIso = appNow().toISOString();
      overrides = [
        ...overrides,
        {
          // Optimistic row. `id` is synthesised so the key stays
          // unique; the next fetch will swap in the real DB row.
          id: `pending-${accountId}-${pickRole}`,
          account_id: accountId,
          employee_id: pickEmp,
          role: pickRole,
          assigned_on: nowIso.slice(0, 10),
          notes: null,
          created_at: nowIso,
        },
      ];
      const next = new Set(removed);
      next.delete(pickRole);
      removed = next;
      pickEmp = '';
      showAdd = false;
    } catch (e) {
      error = String(e);
    } finally {
      busy = false;
    }
  }

  async function remove(role: string): Promise<void> {
    if (busy) return;
    busy = true;
    error = null;
    try {
      await unassignAccountTeamMember({
        account_id: accountId,
        role: role as AccountTeamRole,
        actor_id: actorId,
      });
      const next = new Set(removed);
      next.add(role);
      removed = next;
      overrides = overrides.filter((m) => m.role !== role);
    } catch (e) {
      error = String(e);
    } finally {
      busy = false;
    }
  }
</script>

<Section title="Account team">
    {#if merged.length === 0}
      <p class="empty">No team assigned.</p>
    {:else}
      <ul class="pp-team-list">
        {#each merged as m (`${m.employee_id}-${m.role}`)}
          <li>
            <EntityLink
              kind="employee"
              id={m.employee_id}
              label={empNames.get(m.employee_id)}
              mono={false}
            />
            <span class="pp-team-role">
              {ACCOUNT_TEAM_ROLE_LABEL[m.role as AccountTeamRole] ?? m.role}
            </span>
            <button
              class="pp-team-remove"
              onclick={() => remove(m.role)}
              disabled={busy}
              title={`Unassign ${m.role}`}
            >
              ×
            </button>
          </li>
        {/each}
      </ul>
    {/if}

    {#if availableRoles.length > 0}
      {#if showAdd}
        <div class="pp-team-add">
          <select
            class="pp-team-select"
            bind:value={pickRole}
            disabled={busy}
          >
            {#each availableRoles as r (r)}
              <option value={r}>{ACCOUNT_TEAM_ROLE_LABEL[r]}</option>
            {/each}
          </select>
          <select
            class="pp-team-select"
            bind:value={pickEmp}
            disabled={busy}
          >
            <option value="">— Pick employee —</option>
            {#each employees as e (e.id)}
              <option value={e.id}>{e.name}</option>
            {/each}
          </select>
          <button
            class="pp-team-post"
            onclick={add}
            disabled={!pickEmp || busy}
          >
            {busy ? 'Assigning…' : 'Assign'}
          </button>
          <button
            class="pp-team-cancel"
            onclick={() => {
              showAdd = false;
              error = null;
            }}
            disabled={busy}
          >
            Cancel
          </button>
        </div>
      {:else}
        <button class="pp-team-add-btn" onclick={() => (showAdd = true)}>
          + Add team member
        </button>
      {/if}
    {/if}

    {#if error}<div class="pp-team-error">{error}</div>{/if}
</Section>
