// diagnostic-call.js — Tier 1 of a `field-service` Job.
//
// Turns an untracked Zoom/Teams/Meet session into a searchable
// artifact on the SR: structured metadata + free-text notes +
// optional manually-pasted recording URL. No OAuth. v1 is a form.
// Tier 1 of field-service.
//
// Waivable at triage time; renders a compact read-only state
// when step.status === 'skipped'. Mandatory-at-done: `ended_at` +
// `outcome` required only when flipping to completed.
//
// Waive sends 'skipped' and Close call 'completed' (backlog e639899a):
// they sent 'waived' and 'done', which StepStatus does not have, so the
// PUT refused both with 400 and neither button ever worked. A waive is
// a hand skip, which the PUT admits only for a step no protocol
// describes; a protocol's step answers 409 naming its abort terminal.

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
    // A string child is a Text node by append's definition (backlog
    // 4a359b51 — the reason is at sign-off.js's h()).
    for (const child of children.flat()) {
      if (child == null || child === false) continue;
      el.append(child instanceof Node ? child : String(child));
    }
    return el;
  }

  const CHANNELS = ['zoom', 'teams', 'meet', 'phone', 'other'];

  // The Join button's href, or null when the box holds no http(s) URL
  // (backlog 4a359b51). The box is seeded from step metadata any actor
  // may write, and it was copied into the href verbatim, so a
  // `javascript:` join_url ran in the operator's session on a click —
  // CodeQL's diagnostic-call.js:137 on publish PR #245. The scheme is
  // written here as a literal, never copied from the text; a meeting
  // link is http(s), so nothing a meeting uses is refused.
  function meetingHref(text) {
    let u;
    try {
      u = new URL(String(text).trim());
    } catch (_) {
      return null;
    }
    if (u.protocol === 'https:') return 'https://' + u.href.slice('https://'.length);
    if (u.protocol === 'http:') return 'http://' + u.href.slice('http://'.length);
    return null;
  }

  function mount(container, { step, jobId, onUpdate }) {
    const meta = step.metadata || {};
    const isDone = step.status === 'completed';
    const isWaived = step.status === 'skipped';
    let saving = false;

    const header = h(
      'div',
      { className: 'step-surface-header' },
      h('h3', null, step.title || 'Diagnostic call'),
      h('span', { className: 'step-kind-label' }, 'diagnostic-call'),
      h('span', { className: `step-status step-status-${step.status}` }, step.status),
    );

    if (isWaived) {
      const waivedRoot = h(
        'div',
        { className: 'step-surface step-diagnostic-call' },
        header,
        h('p', { className: 'empty' }, 'Call was waived — triage resolved without a live session.'),
      );
      container.appendChild(waivedRoot);
      return function cleanup() { waivedRoot.remove(); };
    }

    const scheduledInitial = String(meta.scheduled_for || '');
    const scheduledInput = h('input', {
      type: 'datetime-local',
      value: scheduledInitial ? scheduledInitial.slice(0, 16) : '',
      disabled: isDone,
    });
    const channelSelect = h(
      'select',
      { disabled: isDone },
      h('option', { value: '' }, '(pick one)'),
      ...CHANNELS.map((c) => h('option', { value: c }, c)),
    );
    channelSelect.value = String(meta.channel || '');
    const joinUrlInput = h('input', {
      type: 'url',
      value: String(meta.join_url || ''),
      placeholder: 'https://zoom.us/j/...',
      disabled: isDone,
      style: { flex: '1' },
    });
    const joinBtn = h(
      'a',
      {
        target: '_blank',
        rel: 'noopener noreferrer',
        className: 'step-btn step-btn-primary',
        style: { textDecoration: 'none', display: 'inline-block' },
      },
      'Join call →',
    );
    const attendeesInput = h('input', {
      type: 'text',
      value: Array.isArray(meta.attendees) ? meta.attendees.join(', ') : '',
      placeholder: 'emp-042, account-contact-17',
      disabled: isDone,
    });
    const notesInput = h('textarea', {
      rows: 4,
      placeholder: 'What the account showed, what the tech observed, next steps...',
      disabled: isDone,
    });
    notesInput.value = String(meta.notes_md || '');
    const recordingInput = h('input', {
      type: 'url',
      value: String(meta.recording_url || ''),
      placeholder: 'https://... (optional, manually pasted)',
      disabled: isDone,
    });
    const transcriptInput = h('input', {
      type: 'url',
      value: String(meta.transcript_url || ''),
      placeholder: 'https://... (optional)',
      disabled: isDone,
    });
    const outcomeInput = h('input', {
      type: 'text',
      value: String(meta.outcome || ''),
      placeholder: 'Confirmed failure mode, decision, parts needed...',
      disabled: isDone,
    });

    const saveDraftBtn = h('button', { className: 'step-btn' }, 'Save draft');
    const waiveBtn = h('button', { className: 'step-btn' }, 'Waive call');
    const completeBtn = h('button', { className: 'step-btn step-btn-primary' }, 'Close call');
    // A draft save leaves a pending step pending: a step becomes Active
    // only through a claim (design 611fbffd, clause b of backlog 6ef4a36b).
    saveDraftBtn.addEventListener('click', () => save(null));
    waiveBtn.addEventListener('click', () => save('skipped'));
    completeBtn.addEventListener('click', () => save('completed'));

    function updateDerived() {
      const url = meetingHref(joinUrlInput.value);
      if (url) {
        joinBtn.setAttribute('href', url);
        joinBtn.style.display = 'inline-block';
      } else {
        joinBtn.removeAttribute('href');
        joinBtn.style.display = 'none';
      }
      saveDraftBtn.disabled = saving;
      waiveBtn.disabled = saving;
      completeBtn.disabled = saving || outcomeInput.value.trim().length === 0;
    }

    async function save(status) {
      saving = true;
      updateDerived();
      try {
        const scheduledRaw = scheduledInput.value;
        // Two doors, one per kind of write (backlog e39a9d2a, design
        // 93d2bddb): the keys this surface owns go to the step merge
        // door, which merges them against the row as it stands (a null
        // DELETES its key — a cleared box clears it), and the status
        // alone goes to the step PUT. This used to PUT the whole drawn
        // step with `{ ...meta, ownKeys }` as its metadata, and the PUT
        // replaces metadata wholesale, so a key written to the step
        // after this page loaded went back out with the snapshot; the
        // step PUT is closing to any metadata body, so it must not ride
        // there at all. A draft is the merge alone — it sends no status
        // and no holder, so it cannot release a claim (backlog
        // 6ef4a36b). A failed merge moves no status; the host refresh
        // below shows server truth either way, as the one unchecked PUT
        // did before.
        const ownKeys = {
          scheduled_for: scheduledRaw ? new Date(scheduledRaw).toISOString() : null,
          channel: channelSelect.value || null,
          join_url: joinUrlInput.value.trim() || null,
          attendees: attendeesInput.value.split(',').map((s) => s.trim()).filter(Boolean),
          notes_md: notesInput.value || null,
          recording_url: recordingInput.value.trim() || null,
          transcript_url: transcriptInput.value.trim() || null,
          outcome: outcomeInput.value.trim() || null,
          // Stamped once, on Close call, and only when the page drew
          // none: an omitted key is kept by the merge door, so a draft
          // or a Waive leaves whatever end time the step holds.
          ...(status === 'completed' && !meta.ended_at
            ? { ended_at: new Date().toISOString() }
            : {}),
        };
        const merged = await fetch(
          `/api/jobs/${encodeURIComponent(jobId)}/steps/${encodeURIComponent(step.id)}/metadata`,
          {
            method: 'PATCH',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify(ownKeys),
          },
        );
        if (merged.ok && status) {
          await fetch(
            `/api/jobs/${encodeURIComponent(jobId)}/steps/${encodeURIComponent(step.id)}`,
            {
              method: 'PUT',
              headers: { 'Content-Type': 'application/json' },
              body: JSON.stringify({ status }),
            },
          );
        }
        if (onUpdate) onUpdate();
      } finally {
        saving = false;
        updateDerived();
      }
    }

    [joinUrlInput, outcomeInput].forEach((el) => el.addEventListener('input', updateDerived));

    function field(label, input, hint) {
      return h(
        'div',
        {
          className: 'step-field',
          style: { display: 'grid', gridTemplateColumns: '140px 1fr', gap: '8px', alignItems: 'center' },
        },
        h('label', { style: { fontSize: '12px', color: '#78716c' } }, label),
        h(
          'div',
          null,
          input,
          hint ? h('div', { style: { fontSize: '11px', color: '#a8a29e', marginTop: '2px' } }, hint) : null,
        ),
      );
    }

    const form = h(
      'div',
      { className: 'step-field' },
      h('label', null, 'Call details'),
      h(
        'div',
        { style: { display: 'flex', flexDirection: 'column', gap: '8px' } },
        field('Scheduled', scheduledInput),
        field('Channel', channelSelect),
        field(
          'Join URL',
          h('div', { style: { display: 'flex', gap: '8px' } }, joinUrlInput, joinBtn),
        ),
        field('Attendees', attendeesInput, 'Comma-separated. Mix of BOSS employee IDs and account contact IDs.'),
        field('Notes', notesInput, 'Free-text. Markdown renders on the SR 360 view.'),
        field('Recording URL', recordingInput),
        field('Transcript URL', transcriptInput),
        field('Outcome', outcomeInput, 'Required when closing the step.'),
      ),
    );

    const actions = isDone ? null : h(
      'div',
      { className: 'step-actions' },
      saveDraftBtn,
      waiveBtn,
      completeBtn,
    );

    const root = h(
      'div',
      { className: 'step-surface step-diagnostic-call' },
      header,
      form,
      actions,
    );

    updateDerived();
    container.appendChild(root);

    return function cleanup() {
      root.remove();
    };
  }

  if (typeof window.__boss_register_step_plugin !== 'function') {
    console.error('[diagnostic-call-plugin] __boss_register_step_plugin missing');
    return;
  }
  window.__boss_register_step_plugin('diagnostic-call', mount);
})();
