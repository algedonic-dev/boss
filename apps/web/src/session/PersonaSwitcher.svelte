<script lang="ts">
  // Persona switcher — dropdown pinned top-left of every page that
  // lets a Demo Mode visitor view the SPA as any employee. The
  // gateway / dev-server force `role = audit-readonly` regardless of
  // the persona, so this is read-only by construction (see
  // dev-server.ts::proxyApi + cf_access.rs::session_minter).
  //
  // Always visible in Demo Mode (the public playground), so visitors
  // can explore every role's view without flipping a debug toggle.

  import { session, setPersona, type Employee, DEMO_MODE } from './session.svelte';
  import {
    humanizeClassCode,
    type Department,
  } from '../people/types';

  // Demo Mode → always show. Real-tenant deployments still gate on
  // a debug toggle (the original behavior) so it doesn't leak into
  // production — but on a demo deploy DEMO_MODE is the dominant
  // signal.
  let ready = $derived(session.value.kind === 'ready' && DEMO_MODE);
  let currentUserId = $derived(
    session.value.kind === 'ready' ? session.value.user.id : '',
  );

  // Group active employees by department, then dedupe by role
  // within each department so the picker shows one example per
  // role rather than every employee. The brewery roster has ~84
  // packaging-techs, ~72 brewers etc., which made the dropdown
  // hundreds of rows long. With dedup the picker fits on one
  // screen and still covers every distinct role-eye-view a demo
  // visitor might want.
  //
  // Selection rule: first employee encountered per (dept, role).
  // The roster's natural order tends to put the more senior /
  // canonical hire first (executives + heads land before line
  // staff), so the first match is usually the right "exemplar"
  // for that role.
  let byDept = $derived.by(() => {
    const m = new Map<Department, Map<string, Employee>>();
    for (const e of session.roster) {
      if (e.status !== 'active') continue;
      const dept = e.department as Department;
      const byRole = m.get(dept) ?? new Map<string, Employee>();
      if (!byRole.has(e.role)) {
        byRole.set(e.role, e);
      }
      m.set(dept, byRole);
    }
    return [...m.entries()]
      .map(([dept, byRole]) => {
        const exemplars = [...byRole.values()].sort((a, b) =>
          humanizeClassCode(a.role).localeCompare(humanizeClassCode(b.role)),
        );
        return [dept, exemplars] as const;
      })
      .sort((a, b) =>
        humanizeClassCode(a[0]).localeCompare(humanizeClassCode(b[0])),
      );
  });

  function onChange(e: Event): void {
    const v = (e.currentTarget as HTMLSelectElement).value;
    if (v) setPersona(v);
  }

  let helpOpen = $state(false);
</script>

{#if ready}
  <div class="persona-switcher" title="Demo Mode: switch viewing-as persona — always read-only">
    <button
      type="button"
      class="persona-mode-badge"
      aria-haspopup="dialog"
      aria-expanded={helpOpen}
      title="What is Demo Mode? — click for details"
      onclick={() => (helpOpen = !helpOpen)}
    >
      Demo Mode · read-only
    </button>
    <span class="persona-label">viewing as</span>
    <select class="persona-select" value={currentUserId} onchange={onChange}>
      {#each byDept as [dept, emps] (dept)}
        <optgroup label={humanizeClassCode(dept)}>
          {#each emps as e (e.id)}
            <option value={e.id}>
              {e.name} — {humanizeClassCode(e.role)}
            </option>
          {/each}
        </optgroup>
      {/each}
    </select>
  </div>

  {#if helpOpen}
    <div
      class="demo-mode-popover"
      role="dialog"
      aria-label="Demo Mode — overview and how to turn off"
    >
      <button
        type="button"
        class="demo-mode-popover-close"
        aria-label="Close"
        onclick={() => (helpOpen = false)}
      >×</button>
      <h3>Demo Mode is on</h3>
      <p>
        Every visitor gets a read-only synthetic session against a
        live simulator. Pick any role from <em>viewing as</em> to see
        the SPA through that role's eyes. Writes are rejected at the
        policy layer — you can browse but not change state.
      </p>
      <h3>Turning it off (for evaluators)</h3>
      <p>
        To test write paths, sign in with real BOSS auth, or run a
        clean (non-simulated) deployment, switch the gateway out of
        Demo Mode and pause the simulator:
      </p>
      <ol>
        <li>
          Edit the gateway drop-in
          (<code>/etc/systemd/system/boss-gateway.service.d/demo-mode.conf</code>)
          and change <code>BOSS_DEMO_MODE=1</code> to
          <code>BOSS_DEMO_MODE=0</code>, then run
          <code>sudo systemctl daemon-reload &amp;&amp; sudo systemctl restart boss-gateway</code>.
        </li>
        <li>
          Stop the sim so it doesn't keep generating audit-log
          activity:
          <code>sudo systemctl stop boss-brewery-sim</code> (or
          your tenant's sim unit).
        </li>
        <li>
          Sign in at <code>/login</code> with the bootstrap
          credentials printed by <code>boss doctor install</code>
          (or follow the local-auth runbook to mint a new account).
        </li>
      </ol>
      <p class="demo-mode-popover-foot">
        Background: <code>infra/gateway/demo-mode.conf</code> in the
        repo. Source: <code>boss-gateway/src/cf_access.rs</code>'s
        synthetic-session minter.
      </p>
    </div>
  {/if}
{/if}

<style>
  .persona-mode-badge {
    display: inline-block;
    margin-right: 10px;
    padding: 2px 8px;
    border-radius: 4px;
    background: #f59e0b;
    color: #1c1917;
    font-size: 11px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    border: 0;
    cursor: pointer;
    font-family: inherit;
  }
  .persona-mode-badge:hover {
    background: #d97706;
  }
  .persona-mode-badge:focus-visible {
    outline: 2px solid #1c1917;
    outline-offset: 2px;
  }

  .demo-mode-popover {
    /* Anchored to the top-bar area (right of the 200px sidebar
       per styles.css `.app-shell`'s grid-template). Without
       offsetting past the sidebar the popover renders behind
       it and reads as half-cut. */
    position: fixed;
    top: 56px;
    left: 212px;
    z-index: 1000;
    max-width: 420px;
    background: #fffbeb;
    border: 1px solid #f59e0b;
    border-radius: 8px;
    padding: 14px 18px 14px 14px;
    box-shadow: 0 8px 24px rgba(0, 0, 0, 0.15);
    font-size: 13px;
    line-height: 1.5;
    color: #1c1917;
  }
  .demo-mode-popover h3 {
    margin: 0 0 4px 0;
    font-size: 13px;
    font-weight: 700;
  }
  .demo-mode-popover h3:not(:first-child) {
    margin-top: 10px;
  }
  .demo-mode-popover p {
    margin: 0 0 6px 0;
  }
  .demo-mode-popover ol {
    margin: 4px 0 6px 0;
    padding-left: 20px;
  }
  .demo-mode-popover li {
    margin-bottom: 6px;
  }
  .demo-mode-popover code {
    background: #fef3c7;
    padding: 1px 4px;
    border-radius: 3px;
    font-size: 12px;
  }
  .demo-mode-popover-foot {
    margin-top: 8px;
    padding-top: 8px;
    border-top: 1px dashed #fcd34d;
    font-size: 11px;
    color: #57534e;
  }
  .demo-mode-popover-close {
    position: absolute;
    top: 6px;
    right: 8px;
    background: transparent;
    border: 0;
    cursor: pointer;
    font-size: 18px;
    line-height: 1;
    color: #57534e;
    padding: 4px 6px;
  }
  .demo-mode-popover-close:hover {
    color: #1c1917;
  }
</style>
