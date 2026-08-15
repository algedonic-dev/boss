// sign-off.js — the surface for a step that needs someone's name on it.
//
// WHY THIS EXISTS. `sign-off` is the most common step kind across the
// platform protocols — ship-a-change's `gate`, doc-flatten's `review`,
// protocol-retro's `ratify`, incident's `prevention` — and it was the
// one with no plugin, so every one of them rendered the generic step
// surface. David, 2026-08-15 (feedback b1aa1f5f), on the doc-flatten
// ratify step: "Missing custom step UX".
//
// WHAT THE GENERIC SURFACE HIDES, and the reason this is worth a
// bundle rather than a nicer label: a sign-off step carries two facts
// that have no generic rendering. First, `sign_offs_required` is a list
// of ROLES, and the step is not completable until every one has a stamp
// — the generic surface shows a Complete button that simply fails.
// Second, a stamp records the `step_shape_hash` at the moment it was
// made, so editing the step after signing INVALIDATES the stamp; the
// API answers a completion attempt with 409 and a `stale_roles` list.
// That is a good rule (a signature is on a specific thing, not on a
// step id) and it is invisible until it bites, so this surface says it
// out loud instead.
//
// Plugins are plain-DOM mount functions. The host (StepPluginMount
// .svelte) creates a container <div> and calls mount(container, props);
// we render into it and return a cleanup fn. No framework runtime.

(function () {
  function h(tag, attrs, ...children) {
    const el = document.createElement(tag);
    if (attrs) {
      for (const k in attrs) {
        const v = attrs[k];
        if (v == null || v === false) continue;
        if (k === 'className') el.className = v;
        else if (k.startsWith('on') && typeof v === 'function') {
          el.addEventListener(k.slice(2).toLowerCase(), v);
        } else if (k === 'disabled' || k === 'value') {
          el[k] = v;
        } else {
          el.setAttribute(k, String(v));
        }
      }
    }
    for (const child of children.flat()) {
      if (child == null || child === false) continue;
      el.appendChild(child instanceof Node ? child : document.createTextNode(String(child)));
    }
    return el;
  }

  function when(iso) {
    if (!iso) return '';
    const d = new Date(iso);
    return Number.isNaN(d.getTime()) ? String(iso) : d.toLocaleString();
  }

  function mount(container, { step, jobId, onUpdate }) {
    const required = Array.isArray(step.sign_offs_required) ? step.sign_offs_required : [];
    let stamps = Array.isArray(step.sign_offs) ? step.sign_offs.slice() : [];
    // The API's terminal spellings, matching answer-question.js. The
    // older bundles test for 'done', which is not a StepStatus the
    // server ever returns — a legacy spelling that happens to be falsy
    // here and so reads as "not finished".
    const isDone = step.status === 'completed' || step.status === 'skipped';
    let busy = false;
    let error = null;

    const stampFor = (role) => stamps.find((s) => s && s.role === role);
    const outstanding = () => required.filter((r) => !stampFor(r));

    const rolesDiv = h('div', { className: 'step-signoff-roles' });
    const actionsDiv = h('div', { className: 'step-actions' });
    const errorDiv = h('div', { className: 'step-signoff-error' });

    function renderRoles() {
      rolesDiv.replaceChildren();
      if (required.length === 0) {
        // Not an error state: plenty of sign-off steps declare no roles
        // and are just "someone looked at this". Say so, rather than
        // rendering an empty box that reads as a loading failure.
        rolesDiv.appendChild(
          h(
            'p',
            { className: 'step-signoff-none' },
            'No roles are required on this step — completing it is the sign-off.',
          ),
        );
        return;
      }
      required.forEach((role) => {
        const stamp = stampFor(role);
        const row = h(
          'div',
          { className: `step-signoff-role ${stamp ? 'is-signed' : 'is-outstanding'}` },
          h('span', { className: 'step-signoff-rolename' }, role),
          stamp
            ? h(
                'span',
                { className: 'step-signoff-stamp' },
                `signed by ${stamp.authority_id || 'unknown'} · ${when(stamp.stamped_at)}`,
              )
            : h('span', { className: 'step-signoff-await' }, 'awaiting signature'),
          !stamp && !isDone
            ? h(
                'button',
                {
                  className: 'step-btn',
                  disabled: busy,
                  onClick: () => sign(role),
                },
                `Sign off as ${role}`,
              )
            : null,
        );
        rolesDiv.appendChild(row);
      });
    }

    function renderActions() {
      actionsDiv.replaceChildren();
      if (isDone) return;
      const left = outstanding();
      if (left.length > 0) {
        // Deliberately no Complete button while a signature is missing.
        // The API would refuse it, and a button that exists only to
        // produce an error teaches the operator to distrust buttons.
        actionsDiv.appendChild(
          h(
            'span',
            { className: 'step-signoff-blocked' },
            `Cannot complete yet — ${left.length} signature${left.length === 1 ? '' : 's'} outstanding: ${left.join(', ')}`,
          ),
        );
        return;
      }
      actionsDiv.appendChild(
        h(
          'button',
          { className: 'step-btn step-btn-primary', disabled: busy, onClick: complete },
          required.length ? 'All signatures in — complete step' : 'Sign off and complete',
        ),
      );
    }

    function renderError() {
      errorDiv.replaceChildren();
      if (!error) return;
      errorDiv.appendChild(h('p', { className: 'step-error' }, error));
    }

    function renderAll() {
      renderRoles();
      renderActions();
      renderError();
    }

    async function sign(role) {
      busy = true;
      error = null;
      renderAll();
      try {
        const res = await fetch(`/api/jobs/${jobId}/steps/${step.id}/sign-offs`, {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ role }),
        });
        if (!res.ok) {
          error = `Could not record the ${role} signature (${res.status}). ${await res.text()}`;
        } else {
          // Re-read rather than assume: the server decides who the
          // stamp is attributed to and what shape hash it pins, and
          // guessing here would show a signature that does not match
          // the one recorded.
          const fresh = await fetch(`/api/jobs/${jobId}`).then((r) => (r.ok ? r.json() : null));
          const s = fresh && (fresh.steps || []).find((x) => x.id === step.id);
          if (s) stamps = Array.isArray(s.sign_offs) ? s.sign_offs : [];
          if (typeof onUpdate === 'function') onUpdate();
        }
      } catch (e) {
        error = `Could not record the ${role} signature: ${e}`;
      } finally {
        busy = false;
        renderAll();
      }
    }

    async function complete() {
      busy = true;
      error = null;
      renderAll();
      try {
        const res = await fetch(`/api/jobs/${jobId}/steps/${step.id}`, {
          method: 'PUT',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ status: 'completed' }),
        });
        if (!res.ok) {
          // 409 here is the stale-stamp case: the step changed after it
          // was signed, so the stamps no longer apply. Surface the
          // server's own explanation — it names which roles went stale.
          error = `${res.status}: ${await res.text()}`;
        } else if (typeof onUpdate === 'function') {
          onUpdate();
        }
      } catch (e) {
        error = `Could not complete the step: ${e}`;
      } finally {
        busy = false;
        renderAll();
      }
    }

    const root = h(
      'div',
      { className: 'step-signoff' },
      h('div', { className: 'step-signoff-head' }, 'Signatures required'),
      rolesDiv,
      errorDiv,
      actionsDiv,
    );
    renderAll();
    container.appendChild(root);
    return () => root.remove();
  }

  if (typeof window.__boss_register_step_plugin !== 'function') {
    console.error('[sign-off-plugin] __boss_register_step_plugin not on window');
    return;
  }
  window.__boss_register_step_plugin('sign-off', mount);
})();
