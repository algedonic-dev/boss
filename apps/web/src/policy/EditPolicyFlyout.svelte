<script lang="ts">
  // Policy-rule edit modal: one rule's scope, opened from a matrix cell on
  // /it/registry/policy.

  import type { PolicyRule, Scope } from './policyTypes';
  import { departments, departmentsReadFailed } from '@boss/web-kit/session/departments.svelte';
  import { scopeForDisplay, scopeOptions } from './policyView';

  type Props = {
    rule: PolicyRule;
    changedBy: string;
    onClose: () => void;
    onSaved: () => void;
  };
  let { rule, changedBy, onClose, onSaved }: Props = $props();

  // Department scopes come from the departments registry (GET
  // /api/departments) — the list an employee's department is validated
  // against, so a `department:<code>` scope names a code an employee can
  // actually hold. It read the `(employee, department)` Classes until
  // backlog c87e3d6d retired that second list. The core scopes and the
  // rule's own current scope come from ./policyView, so a department the
  // registry does not list (or could not be read for) is still the option
  // that is selected, never a blank the Save would silently rewrite
  // (backlog ba8411da) — and a FAILED read says so, rather than reading as
  // a company with no departments (720d6345).
  let SCOPE_OPTIONS = $derived(
    scopeOptions(departments().map((d) => d.code), scopeForDisplay(rule.scope)),
  );
  let departmentsFailed = $derived(departmentsReadFailed());

  let scope = $state(scopeForDisplay(rule.scope));
  let reason = $state('');
  let saving = $state(false);
  let err = $state<string | null>(null);

  async function save(): Promise<void> {
    if (!reason.trim()) {
      err = 'Reason is required';
      return;
    }
    saving = true;
    err = null;
    try {
      const parsedScope: Scope = scope.startsWith('department:')
        ? { department: scope.slice('department:'.length) }
        : (scope as Scope);
      const body = {
        rule: { ...rule, scope: parsedScope },
        changed_by: changedBy,
      };
      const r = await fetch(`/api/policy/rules/${encodeURIComponent(rule.id)}`, {
        method: 'PUT',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify(body),
      });
      // A refused write names why (authority.rs), and the flyout shows it.
      if (!r.ok) throw new Error(`HTTP ${r.status}: ${await r.text()}`);
      onSaved();
    } catch (e) {
      err = e instanceof Error ? e.message : String(e);
      saving = false;
    }
  }
</script>

<div
  role="dialog"
  aria-modal="true"
  tabindex="-1"
  onclick={onClose}
  onkeydown={(e) => {
    if (e.key === 'Escape') onClose();
  }}
  style="position:fixed; inset:0; background:var(--scrim); display:flex; align-items:center; justify-content:center; z-index:100"
>
  <div
    role="document"
    onclick={(e) => e.stopPropagation()}
    onkeydown={(e) => e.stopPropagation()}
    style="background:var(--ink); border-radius:8px; padding:24px; min-width:420px; max-width:600px"
  >
    <h3 style="margin:0 0 12px">
      Edit: <span class="mono">{rule.role}</span> · {rule.resource} · {rule.action}
    </h3>

    <label style="display:block; margin-bottom:12px">
      Scope
      <select bind:value={scope} style="display:block; width:100%; padding:6px; margin-top:4px; font-size:13px">
        {#each SCOPE_OPTIONS as s (s.value)}
          <option value={s.value}>{s.label}</option>
        {/each}
      </select>
    </label>
    {#if departmentsFailed}
      <p class="empty load-failed" style="margin:-4px 0 12px; font-size:13px">
        Department scopes could not be listed: the departments registry read failed.
      </p>
    {/if}

    <label style="display:block; margin-bottom:12px">
      Reason for change (required — goes to audit log)
      <textarea
        bind:value={reason}
        rows="3"
        style="display:block; width:100%; padding:6px; margin-top:4px; font-size:13px"
      ></textarea>
    </label>

    {#if err}<p role="alert" style="color:var(--err); font-size:13px">{err}</p>{/if}

    <div style="display:flex; justify-content:flex-end; gap:8px">
      <button type="button" class="btn" onclick={onClose} disabled={saving}>Cancel</button>
      <button
        type="button"
        class="btn btn-primary"
        onclick={save}
        disabled={saving}
      >
        {saving ? 'Saving…' : 'Save rule'}
      </button>
    </div>
  </div>
</div>
