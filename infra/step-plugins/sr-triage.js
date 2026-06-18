// sr-triage.js — Tier 0 of a `field-service` Job.
//
// Captures mandatory intake fields (account, device, failure,
// priority) + optional metadata (channel, contact, Jira), then
// records the triage decision (dispatch / remote / parts-only)
// at completion.
//
// Mandatory-at-done semantics match the registry: the step can exist
// with empty metadata; fields are required only when flipping to done.

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

  const PRIORITIES = ['emergency', 'urgent', 'standard', 'scheduled'];
  const CHANNELS = ['phone', 'email', 'web', 'walk-in'];
  const OUTCOMES = [
    { value: 'dispatch', label: 'Dispatch a tech' },
    { value: 'remote', label: 'Resolve remotely' },
    { value: 'parts-only', label: 'Parts-only shipment' },
  ];

  function mount(container, { step, jobId, onUpdate }) {
    const meta = step.metadata || {};
    const isDone = step.status === 'done' || step.status === 'waived';
    let outcome = String(meta.triage_outcome || '');
    let saving = false;

    const accountInput = h('input', { type: 'text', value: String(meta.account_id || ''), placeholder: 'acc-00001', disabled: isDone });
    const deviceInput = h('input', { type: 'text', value: String(meta.device_serial || ''), placeholder: 'SN-00042', disabled: isDone });
    const failureInput = h('textarea', { rows: 3, placeholder: 'What did the account report?', disabled: isDone });
    failureInput.value = String(meta.failure_description || '');
    const prioritySelect = h('select', { disabled: isDone },
      h('option', { value: '' }, '(pick one)'),
      ...PRIORITIES.map((p) => h('option', { value: p }, p)),
    );
    prioritySelect.value = String(meta.priority || '');
    const channelSelect = h('select', { disabled: isDone },
      h('option', { value: '' }, '(none)'),
      ...CHANNELS.map((c) => h('option', { value: c }, c)),
    );
    channelSelect.value = String(meta.intake_channel || '');
    const contactInput = h('input', { type: 'text', value: String(meta.requester_contact_id || ''), placeholder: 'contact-0042 (optional)', disabled: isDone });
    const jiraInput = h('input', { type: 'text', value: String(meta.jira_issue_key || ''), placeholder: 'OPS-00042 (optional)', disabled: isDone });

    const outcomeButtons = OUTCOMES.map((o) => {
      const btn = h('button', { type: 'button', className: 'step-btn' }, o.label);
      btn.dataset.value = o.value;
      btn.addEventListener('click', () => {
        outcome = o.value;
        updateDerived();
      });
      return btn;
    });
    const outcomeHint = h(
      'p',
      { style: { color: '#a8a29e', fontSize: '11px', marginTop: '6px' } },
      'Fill the four required fields above to record a decision.',
    );

    const saveDraftBtn = h('button', { className: 'step-btn' }, 'Save draft');
    const completeBtn = h('button', { className: 'step-btn step-btn-primary' }, 'Complete triage');
    saveDraftBtn.addEventListener('click', () => save(step.status === 'pending' ? 'active' : step.status));
    completeBtn.addEventListener('click', () => save('done'));

    function field(label, input) {
      return h(
        'div',
        {
          className: 'step-field',
          style: { display: 'grid', gridTemplateColumns: '160px 1fr', gap: '8px', alignItems: 'center' },
        },
        typeof label === 'string' ? h('label', { style: { fontSize: '12px', color: '#78716c' } }, label) : label,
        input,
      );
    }
    function requiredLabel(text) {
      return h('span', null, text, h('span', { style: { marginLeft: '4px', color: '#dc2626', fontSize: '11px' } }, '*'));
    }

    const headerOutcomeBadge = h(
      'span',
      {
        style: {
          marginLeft: '8px', padding: '1px 8px', fontSize: '11px',
          background: '#dbeafe', color: '#1e40af', borderRadius: '3px',
        },
      },
    );

    function mandatoryComplete() {
      return accountInput.value.trim().length > 0
        && deviceInput.value.trim().length > 0
        && failureInput.value.trim().length > 0
        && prioritySelect.value.length > 0;
    }
    function canComplete() {
      return mandatoryComplete() && outcome.length > 0;
    }

    function updateDerived() {
      const mand = mandatoryComplete();
      outcomeButtons.forEach((btn) => {
        const selected = btn.dataset.value === outcome;
        btn.disabled = !mand;
        btn.style.fontWeight = selected ? '600' : '400';
        btn.style.background = selected ? '#dbeafe' : '';
        btn.style.borderColor = selected ? '#3b82f6' : '';
      });
      outcomeHint.style.display = mand ? 'none' : 'block';
      saveDraftBtn.disabled = saving;
      completeBtn.disabled = saving || !canComplete();
      headerOutcomeBadge.textContent = isDone && outcome ? outcome : '';
      headerOutcomeBadge.style.display = isDone && outcome ? 'inline-block' : 'none';
    }

    async function save(status) {
      saving = true;
      updateDerived();
      try {
        const nextMeta = {
          ...meta,
          account_id: accountInput.value.trim() || null,
          device_serial: deviceInput.value.trim() || null,
          failure_description: failureInput.value.trim() || null,
          priority: prioritySelect.value || null,
          intake_channel: channelSelect.value || null,
          requester_contact_id: contactInput.value.trim() || null,
          jira_issue_key: jiraInput.value.trim() || null,
          triage_outcome: outcome || null,
        };
        const body = { ...step, job_id: jobId, status: status || step.status, metadata: nextMeta };
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
        updateDerived();
      }
    }

    [accountInput, deviceInput, failureInput, contactInput, jiraInput].forEach((el) => {
      el.addEventListener('input', updateDerived);
    });
    [prioritySelect, channelSelect].forEach((el) => {
      el.addEventListener('change', updateDerived);
    });

    const form = h(
      'div',
      { className: 'step-field' },
      h('label', null, 'Intake details'),
      h(
        'div',
        { style: { display: 'flex', flexDirection: 'column', gap: '8px' } },
        field(requiredLabel('Account ID'), accountInput),
        field(requiredLabel('Device serial'), deviceInput),
        field(requiredLabel('Failure description'), failureInput),
        field(requiredLabel('Priority'), prioritySelect),
        field('Intake channel', channelSelect),
        field('Requester contact', contactInput),
        field('Ops # / Jira', jiraInput),
      ),
    );

    const outcomePicker = isDone ? null : h(
      'div',
      { className: 'step-field' },
      h('label', null, 'Triage decision'),
      h('div', { style: { display: 'flex', gap: '8px', flexWrap: 'wrap' } }, ...outcomeButtons),
      outcomeHint,
    );

    const actions = isDone ? null : h('div', { className: 'step-actions' }, saveDraftBtn, completeBtn);

    const root = h(
      'div',
      { className: 'step-surface step-sr-triage' },
      h(
        'div',
        { className: 'step-surface-header' },
        h('h3', null, step.title || 'Triage request'),
        h('span', { className: 'step-kind-label' }, 'sr-triage'),
        h('span', { className: `step-status step-status-${step.status}` }, step.status),
        headerOutcomeBadge,
      ),
      form,
      outcomePicker,
      actions,
    );

    updateDerived();
    container.appendChild(root);

    return function cleanup() {
      root.remove();
    };
  }

  if (typeof window.__boss_register_step_plugin !== 'function') {
    console.error('[sr-triage-plugin] __boss_register_step_plugin missing');
    return;
  }
  window.__boss_register_step_plugin('sr-triage', mount);
})();
