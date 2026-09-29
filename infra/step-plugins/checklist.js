// checklist.js — renders step.metadata.items as a checkbox list,
// auto-completes the step when every box is checked.
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
        } else if (k === 'checked' || k === 'disabled' || k === 'value') {
          el[k] = v;
        } else {
          el.setAttribute(k, String(v));
        }
      }
    }
    for (const child of children.flat()) {
      if (child == null || child === false) continue;
      // A string child is a Text node by append's definition (backlog
      // 4a359b51 — the reason is at sign-off.js's h()).
      el.append(child instanceof Node ? child : String(child));
    }
    return el;
  }

  // The step's own declaration of who runs it, read the way boss-jobs
  // reads it: `human_only` in either spelling the registry carries
  // (human_only.rs `declaration`), and the agent block as the
  // projection's four keys, all or none (agent_spec.rs `projected`).
  function humanOnly(m) {
    const v = m.human_only;
    return v === true || (typeof v === 'string' && v.trim().toLowerCase() === 'true');
  }
  function agentProfile(m) {
    const text = (k) => typeof m[k] === 'string';
    const whole =
      text('agent_profile') &&
      text('agent_model') &&
      typeof m.agent_budget_usd === 'number' &&
      text('agent_effort');
    return whole && !humanOnly(m) ? m.agent_profile : null;
  }

  // An empty list is not an answer (backlog 82cd3da2, David 2026-09-27):
  // the publish review step of 246d597a was ready, its agent run had not
  // started, and this bundle drew an empty list, a 0/0 and a Save
  // button — on the full step page, which mounts the plugin without the
  // procedure panel StepSurface draws. So it says that nothing has run,
  // who writes the items, and what the procedure says will appear.
  function emptyState(step) {
    const m = (step.metadata && typeof step.metadata === 'object' && step.metadata) || {};
    let who;
    if (step.status === 'completed') {
      who = 'This step completed with no items recorded.';
    } else {
      const profile = agentProfile(m);
      const writer = profile
        ? `an agent run (profile ${profile}) writes them when it is dispatched`
        : step.assignee_id
          ? `${step.assignee_id}, who holds this step, writes them`
          : 'whoever takes this step writes them';
      who = `No items yet: ${writer} — nothing has run.`;
    }
    const procedure =
      step.status !== 'completed' && typeof m.procedure === 'string' && m.procedure.trim()
        ? m.procedure
        : null;
    return h(
      'div',
      { className: 'step-checklist-empty', 'data-testid': 'checklist-empty' },
      h('p', null, who),
      procedure &&
        h(
          'div',
          { 'data-testid': 'checklist-procedure' },
          h('strong', null, 'The procedure that writes them:'),
          h('p', { style: 'white-space: pre-wrap' }, procedure),
        ),
    );
  }

  function mount(container, { step, jobId, onUpdate }) {
    const items = Array.isArray(step.metadata && step.metadata.items)
      ? step.metadata.items.map((i) => ({
          label: String(i.label || ''),
          checked: !!i.checked,
        }))
      : [];
    let saving = false;
    const isDone = step.status === 'completed';

    function allChecked() {
      return items.length > 0 && items.every((i) => i.checked);
    }
    function checkedCount() {
      return items.filter((i) => i.checked).length;
    }

    const progressSpan = h('span', { className: 'step-checklist-progress' });
    const itemsDiv = h('div', { className: 'step-checklist-items' });
    const actionsDiv = h('div', { className: 'step-actions' });

    const saveBtn = h('button', { className: 'step-btn' }, 'Save');
    const completeBtn = h(
      'button',
      { className: 'step-btn step-btn-primary' },
      'All checked — complete step',
    );
    saveBtn.addEventListener('click', () => save(false));
    completeBtn.addEventListener('click', () => save(true));

    function renderItems() {
      itemsDiv.replaceChildren();
      if (items.length === 0) {
        itemsDiv.appendChild(emptyState(step));
        return;
      }
      items.forEach((item, idx) => {
        const row = h(
          'label',
          {
            className: `step-checklist-item ${item.checked ? 'step-checklist-checked' : ''}`,
          },
          h('input', {
            type: 'checkbox',
            checked: item.checked,
            disabled: isDone,
            onChange: () => toggle(idx),
          }),
          h('span', null, item.label),
        );
        itemsDiv.appendChild(row);
      });
    }

    function renderActions() {
      actionsDiv.replaceChildren();
      // Nothing to tick is nothing to save.
      if (isDone || items.length === 0) return;
      saveBtn.disabled = saving;
      actionsDiv.appendChild(saveBtn);
      if (allChecked()) {
        completeBtn.disabled = saving;
        actionsDiv.appendChild(completeBtn);
      }
    }

    function renderProgress() {
      progressSpan.textContent = items.length === 0 ? '' : `${checkedCount()}/${items.length}`;
    }

    function toggle(idx) {
      items[idx] = { ...items[idx], checked: !items[idx].checked };
      renderItems();
      renderProgress();
      renderActions();
    }

    async function save(autoComplete) {
      saving = true;
      renderActions();
      try {
        const completing = autoComplete && allChecked();
        // Merge ONLY the key this surface owns. The old idiom PUT
        // `{ ...step.metadata, items }` — the page-load snapshot plus
        // our key — and PUT metadata is replaced WHOLESALE, so any key
        // another writer added after this page loaded was silently
        // erased (the lost update the step metadata PATCH exists to
        // retire). The server merges against the row as it stands and
        // preserves every key this body does not name.
        const pr = await fetch(`/api/jobs/${jobId}/steps/${step.id}/metadata`, {
          method: 'PATCH',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ items }),
        });
        // The old single PUT went unchecked; with two writes the
        // follow-up must not fire when the merge it depends on failed,
        // or a failed save could still move the status. The host
        // refresh below still runs either way, so a silent failure
        // shows server truth — exactly what it showed before.
        if (pr.ok) {
          if (completing) {
            // The status alone (backlog e39a9d2a, design 93d2bddb).
            // This read the merged row back and PUT it whole with the
            // new status — correct, but a metadata body on the step
            // PUT, and that PUT is closing to any metadata body. The
            // PUT keeps every field a body omits, so the merge above
            // is what the step completes with.
            await fetch(`/api/jobs/${jobId}/steps/${step.id}`, {
              method: 'PUT',
              headers: { 'Content-Type': 'application/json' },
              body: JSON.stringify({ status: 'completed' }),
            });
          }
          // A save no longer flips a pending step active (design
          // 611fbffd, clause b of backlog 6ef4a36b): a step becomes
          // Active only through a claim, and a pending one is opened by
          // its protocol's predicate, not by a save on a page.
        }
        onUpdate();
      } finally {
        saving = false;
        renderActions();
      }
    }

    const root = h(
      'div',
      { className: 'step-surface step-checklist' },
      h(
        'div',
        { className: 'step-surface-header' },
        h('h3', null, step.title),
        h('span', { className: `step-status step-status-${step.status}` }, step.status),
        progressSpan,
      ),
      itemsDiv,
      actionsDiv,
    );

    renderItems();
    renderProgress();
    renderActions();
    container.appendChild(root);

    return function cleanup() {
      root.remove();
    };
  }

  if (typeof window.__boss_register_step_plugin !== 'function') {
    console.error('[checklist-plugin] __boss_register_step_plugin not on window');
    return;
  }
  window.__boss_register_step_plugin('checklist', mount);
})();
