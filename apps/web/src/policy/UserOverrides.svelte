<script lang="ts">
  import { session } from '@boss/web-kit/session/session.svelte';
  import { listOverrides, parseOverride, saveOverride, verifyOverride, type UserOverride, type OverrideResult } from './userOverrides';
  import { scopeForDisplay } from './policyView';

  let id = $state('');
  let user = $state('');
  let action = $state('');
  let resource = $state('');
  let scope = $state('none');
  let department = $state('');
  let reason = $state('');
  let expiry = $state('');
  let prepared = $state('');
  let reviewed = $state<UserOverride | null>(null);
  let result = $state<OverrideResult | null>(null);
  let error = $state<string | null>(null);
  let busy = $state(false);
  let rows = $state<ReadonlyArray<UserOverride> | null>(null);
  let readError = $state<string | null>(null);
  let reading = $state(false);
  let listedUser = $state('');
  let person = $derived(session.value.kind === 'ready' ? session.value.user : null);
  let locked = $derived(busy || result?.kind === 'uncertain');

  function fill(row: UserOverride): void {
    id = row.id; user = row.user_id; action = row.action; resource = row.resource;
    scope = typeof row.scope === 'string' ? row.scope : 'department';
    department = typeof row.scope === 'string' ? '' : row.scope.department;
    reason = row.reason; expiry = row.expires_at ?? '';
    reviewed = null; result = null; error = null;
  }
  function importPrepared(): void {
    try { fill(parseOverride(JSON.parse(prepared))); }
    catch (e) { error = e instanceof Error ? e.message : String(e); }
  }
  function review(): void {
    try {
      reviewed = parseOverride({ id, user_id: user, action, resource, scope: scope === 'department' ? { department } : scope, reason, expires_at: expiry || null });
      result = null; error = null;
    } catch (e) { error = e instanceof Error ? e.message : String(e); }
  }
  async function load(): Promise<void> {
    if (!user.trim()) { readError = 'Enter the person’s employee ID first.'; return; }
    reading = true; rows = null; readError = null;
    const requestedUser = user;
    try { rows = await listOverrides(requestedUser); listedUser = requestedUser; }
    catch (e) { readError = e instanceof Error ? e.message : String(e); }
    finally { reading = false; }
  }
  async function submit(checkOnly = false): Promise<void> {
    if (!reviewed || busy || !person || (!checkOnly && result?.kind === 'uncertain')) return;
    busy = true; error = null;
    result = checkOnly ? await verifyOverride(reviewed, fetch, result?.receipt) : await saveOverride(reviewed);
    busy = false;
    if (result.kind === 'verified') { rows = null; }
  }
  function edit(): void { reviewed = null; result = null; }
  function next(): void {
    fill({ id: '', user_id: user, action: '', resource: '', scope: 'none', reason: '', expires_at: null });
    prepared = '';
  }
  function downloadReceipt(): void {
    if (!result) return;
    const url = URL.createObjectURL(new Blob([JSON.stringify({ ...result.receipt, observed_by: person?.id, verdict: result.kind, error: result.kind === 'verified' ? null : result.error }, null, 2)], { type: 'application/json' }));
    const link = document.createElement('a'); link.href = url; link.download = `permission-${result.receipt.expected.id}.json`; link.click(); URL.revokeObjectURL(url);
  }
</script>

<section class="person-permissions" aria-labelledby="person-permissions-title">
  <h2 id="person-permissions-title">Named-person permissions</h2>
  <p>Review permissions for one employee, or prepare a change. Each permission takes precedence over their role’s rule.</p>
  {#if person}<p>Signed in as <strong>{person.name ?? person.id}</strong> ({person.id}).</p>
  {:else}<p role="alert">Sign in to save a permission.</p>{/if}

  <fieldset disabled={locked || reviewed !== null}>
    <label>Person’s employee ID<input bind:value={user} /></label>
  </fieldset>
  <button class="btn" type="button" onclick={load} disabled={reading || !user.trim() || busy}>View current permissions</button>
  {#if reading}<p>Loading permissions…</p>{/if}
  {#if readError}<p class="load-failed" role="alert">Failed to read permissions: {readError}</p>{/if}
  {#if rows !== null}
    <h3>Current permissions for {listedUser}</h3>
    {#if rows.length === 0}<p>No named-person permissions returned for {listedUser}.</p>
    {:else}
      <table class="override-table">
        <thead><tr><th>Permission ID</th><th>Person</th><th>Action</th><th>Resource</th><th>Scope</th><th>Reason</th><th>Expiry</th><th></th></tr></thead>
        <tbody>{#each rows as row (row.id)}<tr>
          <td>{row.id}</td><td>{row.user_id}</td><td>{row.action}</td><td>{row.resource}</td><td>{scopeForDisplay(row.scope)}</td><td>{row.reason}</td><td>{row.expires_at ?? 'No expiry'}</td>
          <td><button class="btn" type="button" disabled={locked} onclick={() => fill(row)}>Prepare update</button></td>
        </tr>{/each}</tbody>
      </table>
    {/if}
  {/if}

  <fieldset disabled={locked || reviewed !== null}>
    <legend>Prepare permission</legend>
    <label>Permission ID<input bind:value={id} /></label>
    <p>Use the prepared ID to update that permission, or choose a new ID to create one.</p>
    <label>Action<input bind:value={action} /></label>
    <label>Resource<input bind:value={resource} /></label>
    <label>Scope<select bind:value={scope}>
      {#each ['none', 'self', 'territory', 'team', 'all', 'department'] as option}<option value={option}>{option}</option>{/each}
    </select></label>
    {#if scope === 'department'}<label>Department<input bind:value={department} /></label>{/if}
    <label>Reason<textarea bind:value={reason} rows="3"></textarea></label>
    <label>Expiry (optional)<input bind:value={expiry} placeholder="2026-12-01T10:30:00Z" /></label>
    <p>Include the timezone. Leave empty for no expiry.</p>
    <details><summary>Load a prepared permission</summary>
      <label>Prepared permission JSON<textarea bind:value={prepared} rows="5"></textarea></label>
      <button class="btn" type="button" onclick={importPrepared}>Load prepared permission</button>
    </details>
    <button class="btn" type="button" onclick={review}>Review permission</button>
  </fieldset>
  {#if error}<p class="load-failed" role="alert">{error}</p>{/if}
  {#if reviewed}
    <h3>Review this permission before saving</h3>
    <dl><dt>Person</dt><dd>{reviewed.user_id}</dd><dt>Permission ID</dt><dd>{reviewed.id}</dd><dt>Action</dt><dd>{reviewed.action}</dd><dt>Resource</dt><dd>{reviewed.resource}</dd><dt>Scope</dt><dd>{scopeForDisplay(reviewed.scope)}</dd><dt>Reason</dt><dd>{reviewed.reason}</dd><dt>Expiry</dt><dd>{reviewed.expires_at ?? 'No expiry'}</dd></dl>
    {#if result}<button class="btn" type="button" onclick={downloadReceipt}>Download permission receipt</button>{/if}
    {#if result?.kind === 'verified'}
      <p role="status">Saved permission confirmed for {result.override.user_id}: {result.override.action} {result.override.resource}, {scopeForDisplay(result.override.scope)}.</p>
      <button class="btn" type="button" onclick={next}>Prepare another permission</button>
    {:else}
      {#if result}<p class="load-failed" role="alert">{result.error}</p>{/if}
      <button class="btn btn-primary" type="button" onclick={() => submit()} disabled={locked || !person}> {busy ? 'Checking…' : 'Save permission'}</button>
      {#if result?.kind === 'uncertain'}<button class="btn" type="button" onclick={() => submit(true)} disabled={busy}>Check saved permission</button>
      {:else}<button class="btn" type="button" onclick={edit} disabled={busy}>Edit prepared permission</button>{/if}
    {/if}
  {/if}
</section>

<style>
  .person-permissions { margin: 24px; }
  fieldset { border: 1px solid var(--wire); padding: 16px; margin: 16px 0; }
  label { display: block; margin: 12px 0; }
  input, textarea, select { display: block; width: min(100%, 580px); margin-top: 4px; padding: 8px; }
  button { margin: 4px 8px 4px 0; }
  table { width: 100%; border-collapse: collapse; }
  th, td { text-align: left; padding: 8px; border-bottom: 1px solid var(--wire); overflow-wrap: anywhere; }
  dl { display: grid; grid-template-columns: max-content 1fr; gap: 8px 16px; }
  dd { margin: 0; overflow-wrap: anywhere; }
</style>
