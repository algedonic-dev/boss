// marketing-launch.js — Tier 4 of a `marketing-motion` Job.
//
// Editable launch date + channel + notes PLUS an embedded ±14-day
// read of the launch-calendar projection (neighboring launches →
// timing-conflict awareness at decision time).
// Tier 4 of marketing-motion.

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

  const CHANNELS = ['email', 'webinar', 'paid-social', 'event', 'content', 'pr', 'other'];

  function isoDay(d) { return d.toISOString().slice(0, 10); }

  function windowFor(launchDate) {
    if (launchDate) {
      const anchor = new Date(launchDate);
      if (!Number.isNaN(anchor.getTime())) {
        const f = new Date(anchor); f.setDate(f.getDate() - 14);
        const t = new Date(anchor); t.setDate(t.getDate() + 14);
        return { from: isoDay(f), to: isoDay(t) };
      }
    }
    const today = new Date();
    const t = new Date(today); t.setDate(t.getDate() + 30);
    return { from: isoDay(today), to: isoDay(t) };
  }

  function mount(container, { step, jobId, onUpdate }) {
    const meta = step.metadata || {};
    const isDone = step.status === 'done' || step.status === 'waived';
    let saving = false;
    let neighborFetchId = 0;

    const header = h(
      'div',
      { className: 'step-surface-header' },
      h('h3', null, step.title),
      h('span', { className: 'step-kind-label' }, 'marketing-launch'),
      h('span', { className: `step-status step-status-${step.status}` }, step.status),
    );

    const dateInput = h('input', {
      type: 'date',
      value: String(meta.launch_date || ''),
      disabled: isDone,
    });
    const channelSelect = h(
      'select',
      { disabled: isDone },
      h('option', { value: '' }, '(none)'),
      ...CHANNELS.map((c) => h('option', { value: c }, c)),
    );
    channelSelect.value = String(meta.launch_channel || '');
    const notesInput = h('textarea', {
      rows: 3,
      disabled: isDone,
      placeholder: 'Launch-day notes — what went out, who announced it',
    });
    notesInput.value = String(meta.notes || '');

    const saveBtn = h('button', { className: 'step-btn' }, 'Save');
    const launchedBtn = h('button', { className: 'step-btn step-btn-primary' }, 'Mark launched');
    saveBtn.addEventListener('click', () => save(null));
    launchedBtn.addEventListener('click', () => save('done'));

    const neighborLabel = h('label');
    const neighborBody = h('div');

    function updateButtons() {
      saveBtn.disabled = saving;
      launchedBtn.disabled = saving;
    }

    async function save(nextStatus) {
      saving = true;
      updateButtons();
      try {
        const patch = {
          launch_date: dateInput.value || (nextStatus === 'done' ? isoDay(new Date()) : null),
          launch_channel: channelSelect.value || null,
          notes: notesInput.value || null,
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

    async function loadNeighbors() {
      const fetchId = ++neighborFetchId;
      const { from, to } = windowFor(dateInput.value);
      neighborLabel.textContent = `Neighboring launches (${from} → ${to})`;
      neighborBody.replaceChildren(h('p', { className: 'empty' }, 'Loading…'));
      try {
        const qs = new URLSearchParams({ from, to });
        const r = await fetch(`/api/jobs/launch-calendar?${qs.toString()}`);
        if (fetchId !== neighborFetchId) return;
        const body = r.ok ? await r.json() : null;
        const rows = body && Array.isArray(body.data) ? body.data : [];
        const filtered = rows.filter((row) => row.job_id !== jobId);
        renderNeighbors(filtered);
      } catch {
        if (fetchId !== neighborFetchId) return;
        renderNeighbors([]);
      }
    }

    function renderNeighbors(neighbors) {
      neighborBody.replaceChildren();
      if (neighbors.length === 0) {
        neighborBody.appendChild(h(
          'p',
          { className: 'empty' },
          'No other marketing motions in this window.',
        ));
        return;
      }
      const ul = h('ul', {
        style: { listStyle: 'none', padding: '0', margin: '0', fontSize: '12px' },
      });
      for (const n of neighbors.slice(0, 15)) {
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
          h(
            'span',
            { className: 'mono', style: { width: '92px', color: '#78716c' } },
            n.launch_date || 'unscheduled',
          ),
          h(
            'a',
            { href: `/dashboard/jobs/${encodeURIComponent(n.job_id)}`, style: { flex: '1' } },
            n.title,
          ),
          n.launch_channel ? h('span', { style: { color: '#78716c' } }, n.launch_channel) : null,
        ));
      }
      if (neighbors.length > 15) {
        ul.appendChild(h(
          'li',
          { style: { padding: '4px 0', color: '#78716c' } },
          `+${neighbors.length - 15} more — see `,
          h('a', { href: '/dashboard/calendar' }, '/calendar'),
        ));
      }
      neighborBody.appendChild(ul);
    }

    dateInput.addEventListener('change', loadNeighbors);

    const form = h(
      'div',
      { className: 'step-field' },
      h('label', null, 'Launch details'),
      h(
        'div',
        { style: { display: 'grid', gridTemplateColumns: 'auto 1fr', gap: '8px', alignItems: 'center' } },
        h('span', { style: { fontSize: '12px', color: '#78716c' } }, 'Date'),
        dateInput,
        h('span', { style: { fontSize: '12px', color: '#78716c' } }, 'Channel'),
        channelSelect,
        h('span', { style: { fontSize: '12px', color: '#78716c' } }, 'Notes'),
        notesInput,
      ),
    );

    const neighborSection = h(
      'div',
      { className: 'step-field' },
      neighborLabel,
      neighborBody,
    );

    const actions = isDone ? null : h('div', { className: 'step-actions' }, saveBtn, launchedBtn);

    const root = h(
      'div',
      { className: 'step-surface step-marketing-launch' },
      header,
      form,
      neighborSection,
      actions,
    );

    updateButtons();
    container.appendChild(root);
    loadNeighbors();

    return function cleanup() {
      neighborFetchId++;
      root.remove();
    };
  }

  if (typeof window.__boss_register_step_plugin !== 'function') {
    console.error('[marketing-launch-plugin] __boss_register_step_plugin missing');
    return;
  }
  window.__boss_register_step_plugin('marketing-launch', mount);
})();
