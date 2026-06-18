// marketing-attribution.js — Tier 5 of a `marketing-motion` Job.
//
// Read-only rollup (opps, revenue, ack rate) + editable measurement
// window + closing notes. Pulls:
//   - parent Job (for campaign subject id)
//   - parent Job's steps (for the tier-1 brief ack rate)
//   - opportunities linked to the campaign
// Asset downloads are placeholders until later.

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

  const currencyFmt = new Intl.NumberFormat('en-US', {
    style: 'currency',
    currency: 'USD',
    maximumFractionDigits: 0,
  });

  function rollupCard(label, value, sub) {
    return h(
      'div',
      {
        style: {
          padding: '12px',
          border: '1px solid #e7e5e4',
          borderRadius: '4px',
          background: '#fafaf9',
          display: 'flex',
          flexDirection: 'column',
          gap: '4px',
        },
      },
      h('div', { style: {
        fontSize: '11px', color: '#78716c',
        textTransform: 'uppercase', letterSpacing: '0.4px',
      } }, label),
      h('div', { style: { fontSize: '20px', fontWeight: '600', color: '#1c1917' } }, value),
      sub ? h('div', { style: { fontSize: '11px', color: '#78716c' } }, sub) : null,
    );
  }

  function mount(container, { step, jobId, onUpdate }) {
    const meta = step.metadata || {};
    const isDone = step.status === 'done' || step.status === 'waived';
    let saving = false;
    let cancelled = false;

    const header = h(
      'div',
      { className: 'step-surface-header' },
      h('h3', null, step.title),
      h('span', { className: 'step-kind-label' }, 'marketing-attribution'),
      h('span', { className: `step-status step-status-${step.status}` }, step.status),
    );

    const rollupGrid = h('div', {
      style: { display: 'grid', gridTemplateColumns: 'repeat(4, 1fr)', gap: '8px' },
    });
    rollupGrid.appendChild(rollupCard('Opportunities', '…', null));
    rollupGrid.appendChild(rollupCard('Revenue influenced', '…', null));
    rollupGrid.appendChild(rollupCard('Brief ack rate', '—', 'loading'));
    rollupGrid.appendChild(rollupCard('Asset downloads', '—', 'pending asset.downloaded events'));

    const measurementInput = h('input', {
      type: 'number',
      min: '0',
      value: meta.measurement_days != null ? String(meta.measurement_days) : '',
      style: { width: '80px' },
      disabled: isDone,
    });
    const windowInput = h('input', {
      type: 'date',
      value: String(meta.window_closes_at || ''),
      disabled: isDone,
    });
    const notesInput = h('textarea', {
      rows: 4,
      disabled: isDone,
      placeholder: 'What did we learn? Did the motion hit its hypothesis?',
    });
    notesInput.value = String(meta.closing_notes || '');

    const saveBtn = h('button', { className: 'step-btn' }, 'Save');
    const completeBtn = h('button', { className: 'step-btn step-btn-primary' }, 'Close measurement window');
    saveBtn.addEventListener('click', () => save(null));
    completeBtn.addEventListener('click', () => save('done'));

    function updateButtons() {
      saveBtn.disabled = saving;
      completeBtn.disabled = saving;
    }

    async function save(nextStatus) {
      saving = true;
      updateButtons();
      try {
        const patch = {
          closing_notes: notesInput.value,
          measurement_days: measurementInput.value === '' ? null : Number(measurementInput.value),
          window_closes_at: windowInput.value || null,
        };
        const body = {
          ...step,
          job_id: jobId,
          status: nextStatus || step.status,
          metadata: { ...meta, ...patch },
        };
        await fetch(
          `/api/jobs/${encodeURIComponent(jobId)}/steps/${encodeURIComponent(step.id)}`,
          {
            method: 'PUT',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify(body),
          },
        );
        if (onUpdate) onUpdate();
      } finally {
        saving = false;
        updateButtons();
      }
    }

    const oppListSection = h('div', { className: 'step-field' });

    async function loadRollup() {
      let job = null;
      let briefStep = null;
      let opps = [];

      try {
        const jr = await fetch(`/api/jobs/${encodeURIComponent(jobId)}`);
        if (!cancelled && jr.ok) job = await jr.json();
      } catch { /* ignore */ }
      if (cancelled) return;

      try {
        const sr = await fetch(`/api/jobs/${encodeURIComponent(jobId)}/steps`);
        if (!cancelled && sr.ok) {
          const steps = await sr.json();
          if (Array.isArray(steps)) briefStep = steps.find((s) => s.kind === 'marketing-brief') || null;
        }
      } catch { /* ignore */ }
      if (cancelled) return;

      const campaignId = job && job.subject && job.subject.subject_kind === 'campaign'
        ? job.subject.id
        : null;

      if (campaignId) {
        try {
          const or = await fetch(`/api/opportunities?campaign_id=${encodeURIComponent(campaignId)}`);
          if (!cancelled && or.ok) {
            const body = await or.json();
            opps = Array.isArray(body) ? body
              : (body && Array.isArray(body.data)) ? body.data : [];
          }
        } catch { /* ignore */ }
      }
      if (cancelled) return;

      let ackRate = null;
      if (briefStep && briefStep.metadata) {
        const audience = Array.isArray(briefStep.metadata.audience) ? briefStep.metadata.audience : [];
        const acks = Array.isArray(briefStep.metadata.acknowledgements) ? briefStep.metadata.acknowledgements : [];
        if (audience.length > 0) {
          const ackedIds = new Set(acks.map((a) => a.employee_id));
          const numerator = audience.filter((id) => ackedIds.has(id)).length;
          ackRate = { numerator, denominator: audience.length };
        }
      }

      const revenueCents = opps.reduce((sum, o) => sum + (Number(o.expected_revenue_cents) || 0), 0);
      const wonCents = opps
        .filter((o) => o.stage === 'closed-won' || o.status === 'closed-won')
        .reduce((sum, o) => sum + (Number(o.expected_revenue_cents) || 0), 0);

      rollupGrid.replaceChildren(
        rollupCard(
          'Opportunities',
          String(opps.length),
          campaignId ? `campaign ${campaignId}` : 'non-campaign subject',
        ),
        rollupCard(
          'Revenue influenced',
          currencyFmt.format(revenueCents / 100),
          wonCents > 0 ? `${currencyFmt.format(wonCents / 100)} closed-won` : 'expected revenue',
        ),
        rollupCard(
          'Brief ack rate',
          ackRate ? `${ackRate.numerator}/${ackRate.denominator}` : '—',
          ackRate ? null : 'no audience',
        ),
        rollupCard('Asset downloads', '—', 'pending asset.downloaded events'),
      );

      oppListSection.replaceChildren();
      if (opps.length > 0) {
        const ul = h('ul', {
          style: { listStyle: 'none', padding: '0', margin: '0', fontSize: '12px' },
        });
        for (const o of opps.slice(0, 10)) {
          ul.appendChild(h(
            'li',
            {
              style: {
                padding: '4px 0',
                borderBottom: '1px solid #f5f5f4',
                display: 'flex',
                gap: '8px',
                color: '#44403c',
              },
            },
            h('a', { href: `/dashboard/sales/${encodeURIComponent(o.id)}` }, o.id),
            h('span', { style: { flex: '1' } }, o.name || ''),
            h(
              'span',
              { style: { color: '#78716c' } },
              o.expected_revenue_cents
                ? currencyFmt.format(Number(o.expected_revenue_cents) / 100)
                : '',
            ),
          ));
        }
        if (opps.length > 10) {
          ul.appendChild(h(
            'li',
            { style: { padding: '4px 0', color: '#78716c' } },
            `+${opps.length - 10} more`,
          ));
        }
        oppListSection.appendChild(h('label', null, `Linked opportunities (${opps.length})`));
        oppListSection.appendChild(ul);
      }
    }

    const rollup = h(
      'div',
      { className: 'step-field' },
      h('label', null, 'Attribution rollup'),
      rollupGrid,
    );

    const editable = h(
      'div',
      { className: 'step-field' },
      h('label', null, 'Measurement window'),
      h(
        'div',
        { style: { display: 'flex', gap: '8px', alignItems: 'center', flexWrap: 'wrap' } },
        h('label', { style: { fontSize: '12px', color: '#78716c' } }, 'Days open'),
        measurementInput,
        h('label', {
          style: { fontSize: '12px', color: '#78716c', marginLeft: '12px' },
        }, 'Closes'),
        windowInput,
      ),
    );

    const notesSection = h(
      'div',
      { className: 'step-field' },
      h('label', null, 'Closing notes'),
      notesInput,
    );

    const actions = isDone ? null : h(
      'div',
      { className: 'step-actions' },
      saveBtn,
      completeBtn,
    );

    const root = h(
      'div',
      { className: 'step-surface step-marketing-attribution' },
      header,
      rollup,
      editable,
      notesSection,
      oppListSection,
      actions,
    );

    updateButtons();
    container.appendChild(root);
    loadRollup();

    return function cleanup() {
      cancelled = true;
      root.remove();
    };
  }

  if (typeof window.__boss_register_step_plugin !== 'function') {
    console.error('[marketing-attribution-plugin] __boss_register_step_plugin missing');
    return;
  }
  window.__boss_register_step_plugin('marketing-attribution', mount);
})();
