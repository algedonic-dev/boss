// review-design.js — custom Step UX for the design-doc-review JobKind.
//
// Reads step.metadata.doc_path, fetches /api/design/docs/{path} to
// get the design doc + its parsed open questions (### Qn: <title>
// headings under ## Open Questions). Renders a per-question
// resolution textarea. Step completion is GATED on every question
// having a non-empty resolution recorded.
//
// Resolutions are saved as pending-decisions via
// /api/design/pending-decisions; the follow-up
// /api/design/flush-jobs endpoint writes them into the source
// doc's Decision-history section (each release, settled material
// folds into docs/architecture-decisions.md and the source doc is
// deleted). Brings back the "system models its own development"
// workflow that existed pre-2026-05-03.
//
// Plugin contract: window.__boss_register_step_plugin(kind, mount).
// Host calls mount(container, props) with { step, jobId, onUpdate }.

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
      el.appendChild(child instanceof Node ? child : document.createTextNode(String(child)));
    }
    return el;
  }

  function mount(container, { step, jobId, onUpdate }) {
    const docPath = (step.metadata && step.metadata.doc_path) || '';
    // resolutions: [{ anchor, decision }] — anchor matches the
    // question anchor returned by /api/design/docs/{path}
    // (e.g. "Q1", "Q2", ...).
    let resolutions = Array.isArray(step.metadata && step.metadata.resolutions)
      ? step.metadata.resolutions.map((r) => ({
          anchor: String(r.anchor || ''),
          decision: String(r.decision || ''),
        }))
      : [];

    let doc = null;
    let questions = [];
    let loadError = null;
    let saving = false;
    let saveError = null;
    const isDone = step.status === 'completed' || step.status === 'done';

    const headerDiv = h('div', { className: 'step-surface-header' });
    const bodyDiv = h('div', { className: 'step-review-body' });
    const actionsDiv = h('div', { className: 'step-actions' });

    function resolutionFor(anchor) {
      const r = resolutions.find((x) => x.anchor === anchor);
      return r ? r.decision : '';
    }

    function setResolution(anchor, decision) {
      const idx = resolutions.findIndex((x) => x.anchor === anchor);
      if (idx >= 0) {
        resolutions[idx] = { anchor, decision };
      } else {
        resolutions.push({ anchor, decision });
      }
      renderActions();
      renderProgress();
    }

    function answeredCount() {
      return questions.filter((q) => resolutionFor(q.anchor).trim().length > 0).length;
    }
    function allAnswered() {
      return questions.length > 0 && answeredCount() === questions.length;
    }

    const progressSpan = h('span', { className: 'step-review-progress' });

    function renderProgress() {
      if (loadError) {
        progressSpan.textContent = '';
        return;
      }
      progressSpan.textContent = questions.length
        ? `${answeredCount()}/${questions.length} questions addressed`
        : 'no open questions';
    }

    function renderHeader() {
      headerDiv.replaceChildren(
        h('h3', null, step.title),
        h('span', { className: `step-status step-status-${step.status}` }, step.status),
        progressSpan,
      );
    }

    function renderBody() {
      bodyDiv.replaceChildren();
      if (loadError) {
        bodyDiv.appendChild(
          h('div', { className: 'step-review-error' }, `Failed to load doc: ${loadError}`),
        );
        return;
      }
      if (!doc) {
        bodyDiv.appendChild(h('div', { className: 'step-review-loading' }, 'Loading…'));
        return;
      }
      bodyDiv.appendChild(
        h(
          'div',
          { className: 'step-review-meta' },
          h('strong', null, doc.title || docPath),
          h('span', { className: 'step-review-path' }, ` — ${doc.path}`),
        ),
      );
      // The doc itself — the review is unreadable without it. Server-
      // rendered HTML (pulldown_cmark over the repo-committed markdown;
      // same trust domain as this bundle), collapsible so the questions
      // stay reachable on long docs.
      if (doc.content_html) {
        const details = h('details', { className: 'step-review-doc', open: true });
        details.appendChild(h('summary', null, `Read the doc (${doc.word_count || '—'} words)`));
        const docBody = h('div', { className: 'step-review-doc-body' });
        docBody.innerHTML = doc.content_html;
        details.appendChild(docBody);
        bodyDiv.appendChild(details);
      }
      if (questions.length === 0) {
        bodyDiv.appendChild(
          h(
            'div',
            { className: 'step-review-empty' },
            'No open questions parsed from this doc. The doc is ready to mark reviewed.',
          ),
        );
        return;
      }
      questions.forEach((q) => {
        const ta = h('textarea', {
          rows: 3,
          placeholder: 'Record the decision, deferral, or rationale…',
          disabled: isDone,
          value: resolutionFor(q.anchor),
          onInput: (e) => setResolution(q.anchor, e.target.value),
        });
        const block = h(
          'div',
          {
            className: `step-review-question ${resolutionFor(q.anchor).trim() ? 'step-review-addressed' : ''}`,
          },
          h(
            'div',
            { className: 'step-review-question-header' },
            h('span', { className: 'step-review-question-anchor' }, q.anchor),
            h('span', { className: 'step-review-question-title' }, q.title),
          ),
          // body_html is rendered server-side with the doc pipeline;
          // the <pre> of raw markdown stays only as the fallback for an
          // old docs-api.
          (() => {
            if (q.body_html) {
              const b = h('div', { className: 'step-review-question-body' });
              b.innerHTML = q.body_html;
              return b;
            }
            return q.body_md
              ? h('pre', { className: 'step-review-question-body' }, q.body_md)
              : null;
          })(),
          h('label', { className: 'step-review-resolution-label' }, 'Resolution'),
          ta,
        );
        bodyDiv.appendChild(block);
      });
    }

    function renderActions() {
      actionsDiv.replaceChildren();
      if (saveError) {
        actionsDiv.appendChild(
          h(
            'div',
            { className: 'step-review-error' },
            `Save failed: ${saveError}`,
          ),
        );
      }
      if (isDone) return;
      const saveBtn = h(
        'button',
        { className: 'step-btn', disabled: saving },
        'Save progress',
      );
      saveBtn.addEventListener('click', () => save(false));
      actionsDiv.appendChild(saveBtn);
      if (allAnswered() || questions.length === 0) {
        const doneBtn = h(
          'button',
          { className: 'step-btn step-btn-primary', disabled: saving },
          questions.length === 0
            ? 'Mark reviewed (no questions)'
            : 'All addressed — complete review',
        );
        doneBtn.addEventListener('click', () => save(true));
        actionsDiv.appendChild(doneBtn);
      } else if (questions.length > 0) {
        actionsDiv.appendChild(
          h(
            'span',
            { className: 'step-review-gate-hint' },
            `Complete is gated on every question having a resolution (${answeredCount()}/${questions.length} done).`,
          ),
        );
      }
    }

    async function persistPendingDecisions() {
      // Mirror each non-empty resolution to /api/design/pending-decisions
      // so the existing flush-jobs path can extract them to ADRs. We
      // POST one at a time — the endpoint is upsert-style.
      // PendingDecisionInput wants {doc_path, anchor, kind, resolution}.
      // The reviewer types free-text decisions here (there's no parsed
      // proposal being accepted), so every row is an Override. The old
      // body sent `proposal` with no kind — a 422 this catch swallowed,
      // so flush-jobs always saw zero pending decisions.
      const writes = resolutions
        .filter((r) => r.decision.trim().length > 0)
        .map((r) =>
          fetch('/api/design/pending-decisions', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({
              doc_path: docPath,
              anchor: r.anchor,
              kind: 'override',
              resolution: r.decision,
            }),
          }),
        );
      const results = await Promise.allSettled(writes);
      const failed = results.filter((r) => r.status === 'rejected' || (r.value && !r.value.ok));
      if (failed.length > 0) {
        // Don't block step save on a pending-decision write failure;
        // the resolution is still persisted on the step itself.
        console.warn('[review-design] pending-decisions writes failed:', failed.length);
      }
    }

    async function putStep(status, metadata) {
      const r = await fetch(`/api/jobs/${jobId}/steps/${step.id}`, {
        method: 'PUT',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ ...step, job_id: jobId, status, metadata }),
      });
      if (!r.ok) throw new Error(`step save HTTP ${r.status}: ${await r.text()}`);
    }

    async function save(autoComplete) {
      saving = true;
      saveError = null;
      renderActions();
      try {
        await persistPendingDecisions();
        const completing = autoComplete && (allAnswered() || questions.length === 0);
        const workingStatus = step.status === 'pending' ? 'active' : step.status;
        const finalMeta = { ...step.metadata, doc_path: docPath, resolutions };

        // 1. Persist the FINAL shape first (title + metadata are what
        //    sign-off stamps attest — a stamp taken before the last
        //    metadata write goes stale and the completion 409s).
        await putStep(workingStatus, finalMeta);

        if (completing) {
          // 2. Stamp every required sign-off role in the step's now-
          //    final shape. Policy gates each on `step-signoff:<role>`
          //    — a 403 here means the signed-in user lacks that
          //    authority, and we SAY so instead of silently dropping
          //    it (the pre-fix flow swallowed the completion 409 and
          //    "Mark reviewed" appeared to do nothing).
          for (const role of step.sign_offs_required || []) {
            const r = await fetch(
              `/api/jobs/${jobId}/steps/${step.id}/sign-offs`,
              {
                method: 'POST',
                headers: { 'Content-Type': 'application/json' },
                body: JSON.stringify({ role }),
              },
            );
            if (!r.ok) {
              throw new Error(
                `sign-off as ${role} failed (HTTP ${r.status}): ${await r.text()}`,
              );
            }
          }
          // 3. Complete with the identical metadata the stamps attest.
          await putStep('completed', finalMeta);
        }
        onUpdate();
      } catch (e) {
        saveError = e instanceof Error ? e.message : String(e);
      } finally {
        saving = false;
        renderActions();
      }
    }

    async function load() {
      if (!docPath) {
        loadError = 'step.metadata.doc_path is empty';
        renderBody();
        renderProgress();
        renderActions();
        return;
      }
      try {
        const r = await fetch(`/api/design/docs/${docPath}`);
        if (!r.ok) throw new Error(`HTTP ${r.status}: ${await r.text()}`);
        const detail = await r.json();
        doc = detail;
        questions = Array.isArray(detail.questions) ? detail.questions : [];
      } catch (e) {
        loadError = e instanceof Error ? e.message : String(e);
      }
      renderBody();
      renderProgress();
      renderActions();
    }

    const root = h(
      'div',
      { className: 'step-surface step-review-design' },
      headerDiv,
      bodyDiv,
      actionsDiv,
    );

    renderHeader();
    renderProgress();
    renderBody();
    renderActions();
    container.appendChild(root);
    void load();

    return function cleanup() {
      root.remove();
    };
  }

  if (typeof window.__boss_register_step_plugin !== 'function') {
    console.error('[review-design-plugin] __boss_register_step_plugin not on window');
    return;
  }
  window.__boss_register_step_plugin('review-design', mount);
})();
