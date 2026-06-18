// marketing-brief.js — Tier 1 of a `marketing-motion` Job: brief body
// + audience + per-employee ack tracker. Tier 1 of marketing-motion.
//
// Two modes keyed off metadata.circulated_at:
//   pre-circulate (owner editing) — body + audience picker + Circulate
//   post-circulate — read-only body + ack affordance / tracker;
//                    Mark complete once every audience member acks.

(function () {
  function h(tag, attrs, ...children) {
    const el = document.createElement(tag);
    if (attrs) {
      for (const k in attrs) {
        const v = attrs[k];
        if (v == null || v === false) continue;
        if (k === 'className') el.className = v;
        else if (k === 'style' && typeof v === 'object') Object.assign(el.style, v);
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
      el.appendChild(child instanceof Node ? child : document.createTextNode(String(child)));
    }
    return el;
  }

  function mount(container, { step, jobId, onUpdate, currentUser }) {
    const meta = step.metadata || {};
    const acks = Array.isArray(meta.acknowledgements) ? meta.acknowledgements : [];
    const ackedIds = new Set(acks.map((a) => a.employee_id));
    const isCirculated = Boolean(meta.circulated_at);
    const isDone = step.status === 'done' || step.status === 'waived';
    const viewerIsOwner = !!(currentUser && (
      currentUser.id === step.assignee_id || currentUser.id === meta.owner_id
    ));
    let saving = false;

    const header = h(
      'div',
      { className: 'step-surface-header' },
      h('h3', null, step.title),
      h('span', { className: 'step-kind-label' }, 'marketing-brief'),
      h('span', { className: `step-status step-status-${step.status}` }, step.status),
    );

    async function persist(patch) {
      saving = true;
      try {
        const body = {
          ...step,
          job_id: jobId,
          status: patch.status || step.status,
          metadata: { ...meta, ...(patch.metadata || {}) },
        };
        await fetch(`/api/jobs/${jobId}/steps/${step.id}`, {
          method: 'PUT',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify(body),
        });
        if (onUpdate) onUpdate();
      } finally {
        saving = false;
      }
    }

    // ------------------ Pre-circulate (owner) ------------------
    if (!isCirculated && !isDone && viewerIsOwner) {
      let audience = Array.isArray(meta.audience) ? meta.audience.map(String) : [];

      const bodyInput = h('textarea', {
        rows: 8,
        placeholder: 'What is this motion? Who is the audience? When is launch? What should each department do?',
      });
      bodyInput.value = String(meta.brief_md || '');

      const chipsDiv = h('div', {
        style: { display: 'flex', gap: '6px', flexWrap: 'wrap', marginBottom: '6px' },
      });
      const addInput = h('input', {
        type: 'text',
        placeholder: 'emp-012',
        style: { flex: '1' },
      });
      const addBtn = h('button', { type: 'button', className: 'step-btn' }, 'Add');
      addInput.addEventListener('keydown', (e) => {
        if (e.key === 'Enter') { e.preventDefault(); addAudience(); }
      });
      addBtn.addEventListener('click', addAudience);

      function renderChips() {
        chipsDiv.replaceChildren();
        for (const id of audience) {
          const removeBtn = h('button', {
            type: 'button',
            'aria-label': `Remove ${id}`,
            style: { border: 'none', background: 'none', color: '#78716c', cursor: 'pointer', padding: '0' },
          }, '×');
          removeBtn.addEventListener('click', () => {
            audience = audience.filter((x) => x !== id);
            renderChips();
            updateButtons();
          });
          const chip = h('span', {
            style: {
              padding: '2px 8px', background: '#e7e5e4', borderRadius: '3px',
              fontSize: '12px', display: 'inline-flex', gap: '4px', alignItems: 'center',
            },
          }, id, removeBtn);
          chipsDiv.appendChild(chip);
        }
      }
      function addAudience() {
        const v = addInput.value.trim();
        if (!v || audience.includes(v)) return;
        audience = [...audience, v];
        addInput.value = '';
        renderChips();
        updateButtons();
      }

      const saveDraftBtn = h('button', { className: 'step-btn' }, 'Save draft');
      const circulateBtn = h('button', { className: 'step-btn step-btn-primary' }, 'Circulate');
      saveDraftBtn.addEventListener('click', async () => {
        await persist({
          metadata: { brief_md: bodyInput.value, audience },
          status: step.status === 'pending' ? 'active' : step.status,
        });
        updateButtons();
      });
      circulateBtn.addEventListener('click', async () => {
        if (!bodyInput.value.trim() || audience.length === 0) return;
        await persist({
          metadata: {
            brief_md: bodyInput.value,
            audience,
            circulated_at: new Date().toISOString(),
          },
          status: 'active',
        });
        updateButtons();
      });
      function updateButtons() {
        saveDraftBtn.disabled = saving;
        circulateBtn.disabled = saving || !bodyInput.value.trim() || audience.length === 0;
      }
      bodyInput.addEventListener('input', updateButtons);

      const root = h(
        'div',
        { className: 'step-surface step-marketing-brief' },
        header,
        h(
          'div',
          { className: 'step-field' },
          h('label', null, 'Brief body (markdown)'),
          bodyInput,
        ),
        h(
          'div',
          { className: 'step-field' },
          h('label', null, 'Audience (employee IDs)'),
          chipsDiv,
          h('div', { style: { display: 'flex', gap: '6px' } }, addInput, addBtn),
        ),
        h('div', { className: 'step-actions' }, saveDraftBtn, circulateBtn),
      );

      renderChips();
      updateButtons();
      container.appendChild(root);
      return function cleanup() { root.remove(); };
    }

    // ------------------ Circulated (read-only + ack) ------------------
    const audienceList = Array.isArray(meta.audience) ? meta.audience.map(String) : [];
    const viewerInAudience = !!(currentUser && audienceList.includes(currentUser.id));
    const viewerHasAcked = !!(currentUser && ackedIds.has(currentUser.id));
    const allAcked = audienceList.length > 0 && audienceList.every((id) => ackedIds.has(id));

    header.appendChild(h(
      'span',
      {
        className: 'step-meta-row small',
        style: { marginLeft: '8px', color: '#78716c' },
      },
      `${ackedIds.size}/${audienceList.length} acknowledged`,
    ));

    const briefBody = String(meta.brief_md || '');
    const briefEl = h(
      'div',
      {
        className: 'step-marketing-brief-body',
        style: {
          whiteSpace: 'pre-wrap',
          padding: '12px',
          border: '1px solid #e7e5e4',
          borderRadius: '4px',
          background: '#fafaf9',
          fontSize: '13px',
        },
      },
      briefBody || h('em', { style: { color: '#a8a29e' } }, 'No body yet.'),
    );

    const ackListItems = audienceList.map((id) => {
      const ack = acks.find((a) => a.employee_id === id);
      return h(
        'li',
        {
          style: {
            padding: '3px 0',
            color: ack ? '#16a34a' : '#78716c',
            display: 'flex',
            gap: '8px',
          },
        },
        h('span', null, ack ? '✓' : '○'),
        h('span', null, id),
        ack ? h(
          'span',
          { style: { color: '#78716c', marginLeft: 'auto' } },
          new Date(ack.acknowledged_at).toISOString().slice(0, 10),
        ) : null,
      );
    });

    const actionsRow = h('div', { className: 'step-actions' });
    if (!isDone && viewerInAudience && !viewerHasAcked) {
      const ackBtn = h('button', { className: 'step-btn step-btn-primary' }, 'Acknowledge');
      ackBtn.addEventListener('click', async () => {
        if (!currentUser) return;
        ackBtn.disabled = true;
        await persist({
          metadata: {
            acknowledgements: [
              ...acks,
              { employee_id: currentUser.id, acknowledged_at: new Date().toISOString() },
            ],
          },
        });
      });
      actionsRow.appendChild(ackBtn);
    } else if (!isDone && viewerHasAcked) {
      actionsRow.appendChild(h(
        'span',
        { style: { color: '#16a34a', fontSize: '12px' } },
        '✓ You acknowledged this brief',
      ));
    }
    if (!isDone && viewerIsOwner && allAcked) {
      const completeBtn = h(
        'button',
        { className: 'step-btn step-btn-primary' },
        'All acknowledged — complete step',
      );
      completeBtn.addEventListener('click', async () => {
        completeBtn.disabled = true;
        await persist({ status: 'done' });
      });
      actionsRow.appendChild(completeBtn);
    }

    const root = h(
      'div',
      { className: 'step-surface step-marketing-brief' },
      header,
      h(
        'div',
        { className: 'step-field' },
        h('label', null, 'Brief'),
        briefEl,
      ),
      h(
        'div',
        { className: 'step-field' },
        h('label', null, `Acknowledgements (${ackedIds.size} of ${audienceList.length})`),
        h(
          'ul',
          { style: { listStyle: 'none', padding: '0', fontSize: '12px', color: '#44403c' } },
          ...ackListItems,
        ),
      ),
      actionsRow.childNodes.length ? actionsRow : null,
    );

    container.appendChild(root);
    return function cleanup() { root.remove(); };
  }

  if (typeof window.__boss_register_step_plugin !== 'function') {
    console.error('[marketing-brief-plugin] __boss_register_step_plugin missing');
    return;
  }
  window.__boss_register_step_plugin('marketing-brief', mount);
})();
