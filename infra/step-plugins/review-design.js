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
  // ---------------------------------------------------------------
  // Self-contained styling.
  //
  // The markup below used semantic `.step-review-*` class names that
  // nothing styled: core's stylesheet only carries the generic
  // `.step-surface` wrapper, and a plugin has no business adding rules
  // to core anyway — that is the whole point of shipping UX as a
  // bundle. So the surface rendered at browser defaults: full-width
  // unmeasured prose, bare textareas, no hierarchy. Readable in the
  // sense that the characters were present.
  //
  // Injected once, scoped under `.step-review-design`, and written
  // against core's CSS custom properties (with fallbacks) so it
  // inherits the tenant's light/dark theme instead of fighting it.
  // ---------------------------------------------------------------
  const STYLE_ID = 'boss-review-design-styles';
  const STYLES = `
.step-review-design { --srd-gap: 20px; }

/* Header: title, status, and the progress meter. */
.step-review-design .srd-head {
  display: flex; align-items: baseline; gap: 12px; flex-wrap: wrap;
  padding-bottom: 12px; margin-bottom: var(--srd-gap);
  border-bottom: 1px solid var(--border, #e7e5e4);
}
.step-review-design .srd-head h3 { margin: 0; font-size: 17px; flex: 1 1 auto; }
.step-review-design .srd-progress {
  display: flex; align-items: center; gap: 8px;
  font-size: 12px; color: var(--text-dim, #78716c); white-space: nowrap;
}
.step-review-design .srd-meter {
  width: 120px; height: 6px; border-radius: 3px;
  background: var(--border, #e7e5e4); overflow: hidden;
}
.step-review-design .srd-meter > i {
  display: block; height: 100%; width: 0%;
  background: var(--accent, #2563eb); transition: width .2s ease;
}
.step-review-design .srd-meter.is-complete > i { background: #16a34a; }

/* Two panes: the document to read, the decisions to record. */
.step-review-design .srd-panes {
  display: grid; grid-template-columns: minmax(0, 1fr) minmax(320px, 420px);
  gap: var(--srd-gap); align-items: start;
}
@media (max-width: 1100px) {
  .step-review-design .srd-panes { grid-template-columns: minmax(0, 1fr); }
  .step-review-design .srd-rail { position: static !important; max-height: none !important; }
}

/* Document pane — the reading surface. A measure, a line-height, and
   room to breathe; this is the half that was unreadable. */
.step-review-design .srd-doc {
  background: var(--card, #fff); border: 1px solid var(--border, #e7e5e4);
  border-radius: 8px; padding: 28px 32px;
  max-height: 78vh; overflow-y: auto;
}
.step-review-design .srd-doc-inner { max-width: 68ch; }
.step-review-design .srd-doc-inner > * { max-width: 100%; }
.step-review-design .srd-doc-inner p,
.step-review-design .srd-doc-inner li {
  font-size: 15px; line-height: 1.7; color: var(--text, #1c1917);
}
.step-review-design .srd-doc-inner h1 { font-size: 22px; margin: 0 0 4px; line-height: 1.3; }
.step-review-design .srd-doc-inner h2 {
  font-size: 17px; margin: 32px 0 10px; padding-top: 14px;
  border-top: 1px solid var(--border, #e7e5e4); line-height: 1.35;
}
.step-review-design .srd-doc-inner h3 { font-size: 15px; margin: 22px 0 6px; line-height: 1.4; }
.step-review-design .srd-doc-inner code {
  font-size: 0.9em; padding: 1px 4px; border-radius: 3px;
  background: var(--bg, #f5f5f4);
}
.step-review-design .srd-doc-inner pre {
  background: var(--bg, #f5f5f4); padding: 12px 14px; border-radius: 6px;
  overflow-x: auto; font-size: 13px; line-height: 1.55;
}
.step-review-design .srd-doc-inner blockquote {
  margin: 16px 0; padding: 2px 0 2px 16px;
  border-left: 3px solid var(--accent, #2563eb); color: var(--text-dim, #78716c);
}
.step-review-design .srd-doc-inner table { border-collapse: collapse; font-size: 14px; }
.step-review-design .srd-doc-inner th,
.step-review-design .srd-doc-inner td {
  border: 1px solid var(--border, #e7e5e4); padding: 6px 10px; text-align: left;
}
.step-review-design .srd-docmeta {
  font-size: 12px; color: var(--text-dim, #78716c);
  margin-bottom: 18px; padding-bottom: 10px;
  border-bottom: 1px solid var(--border, #e7e5e4);
}

/* Decision rail — sticky so the questions stay put while you read. */
.step-review-design .srd-rail {
  position: sticky; top: 12px;
  max-height: 78vh; overflow-y: auto;
  display: flex; flex-direction: column; gap: 12px;
}
.step-review-design .srd-rail-title {
  font-size: 12px; font-weight: 600; letter-spacing: .04em;
  text-transform: uppercase; color: var(--text-dim, #78716c);
}
.step-review-design .srd-q {
  border: 1px solid var(--border, #e7e5e4); border-left: 3px solid var(--border, #e7e5e4);
  border-radius: 6px; padding: 14px 16px; background: var(--card, #fff);
}
.step-review-design .srd-q.is-addressed { border-left-color: #16a34a; }
.step-review-design .srd-q-head { display: flex; gap: 8px; align-items: baseline; }
.step-review-design .srd-anchor {
  font-size: 11px; font-weight: 700; padding: 1px 6px; border-radius: 3px;
  background: var(--bg, #f5f5f4); color: var(--text-dim, #78716c); flex: none;
}
.step-review-design .srd-q.is-addressed .srd-anchor { background: #dcfce7; color: #15803d; }
.step-review-design .srd-q-title { font-size: 14px; font-weight: 600; line-height: 1.4; }
.step-review-design .srd-q-body {
  font-size: 13px; line-height: 1.6; color: var(--text-dim, #78716c);
  margin: 8px 0 0; max-height: 8.5em; overflow-y: auto;
}
.step-review-design .srd-q-body p { margin: 0 0 8px; }
.step-review-design .srd-label {
  display: block; font-size: 11px; font-weight: 600; letter-spacing: .04em;
  text-transform: uppercase; color: var(--text-dim, #78716c); margin: 12px 0 4px;
}
.step-review-design .srd-q textarea {
  width: 100%; box-sizing: border-box; resize: vertical;
  font: inherit; font-size: 13px; line-height: 1.55;
  padding: 8px 10px; border-radius: 5px;
  border: 1px solid var(--border, #e7e5e4);
  background: var(--bg, #fafaf9); color: var(--text, #1c1917);
}
.step-review-design .srd-q textarea:focus {
  outline: 2px solid var(--accent, #2563eb); outline-offset: -1px; background: var(--card, #fff);
}
.step-review-design .srd-q textarea:disabled { opacity: .7; }

.step-review-design .srd-empty,
.step-review-design .srd-loading {
  padding: 20px; border-radius: 6px; background: var(--bg, #f5f5f4);
  color: var(--text-dim, #78716c); font-size: 14px;
}
.step-review-design .srd-error {
  padding: 12px 14px; border-radius: 6px; font-size: 13px; line-height: 1.5;
  background: #fef2f2; border: 1px solid #fecaca; color: #b91c1c;
}
.step-review-design .step-actions { margin-top: var(--srd-gap); display: flex; gap: 10px; }
`;

  function injectStyles() {
    if (document.getElementById(STYLE_ID)) return;
    const el = document.createElement('style');
    el.id = STYLE_ID;
    el.textContent = STYLES;
    document.head.appendChild(el);
  }

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

    const headerDiv = h('div', { className: 'srd-head' });
    const bodyDiv = h('div', { className: 'srd-panes' });
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

    const progressSpan = h('span', { className: 'srd-progress' });

    function renderProgress() {
      progressSpan.replaceChildren();
      if (loadError) return;
      if (!questions.length) {
        progressSpan.appendChild(h('span', null, 'no open questions'));
        return;
      }
      const done = answeredCount();
      const meter = h('span', {
        className: `srd-meter ${done === questions.length ? 'is-complete' : ''}`,
      });
      const fill = h('i');
      fill.style.width = `${Math.round((done / questions.length) * 100)}%`;
      meter.appendChild(fill);
      progressSpan.appendChild(meter);
      progressSpan.appendChild(
        h('span', null, `${done}/${questions.length} addressed`),
      );
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
          h('div', { className: 'srd-error' }, `Failed to load doc: ${loadError}`),
        );
        return;
      }
      if (!doc) {
        bodyDiv.appendChild(h('div', { className: 'srd-loading' }, 'Loading the doc…'));
        return;
      }

      // ---- Pane 1: the document, as something to actually read. ----
      // Always open. It was behind a collapsed <details> summarised as
      // "Read the doc" — one more click between a reviewer and the
      // thing they are reviewing.
      const docPane = h('div', { className: 'srd-doc' });
      const inner = h('div', { className: 'srd-doc-inner' });
      inner.appendChild(
        h(
          'div',
          { className: 'srd-docmeta' },
          `${doc.path} · ${doc.status} · ${doc.word_count || '—'} words`,
        ),
      );
      if (doc.content_html) {
        const prose = h('div');
        // Server-rendered from the repo-committed markdown by the same
        // pulldown_cmark pipeline that renders the design page — same
        // trust domain as this bundle.
        prose.innerHTML = doc.content_html;
        inner.appendChild(prose);
      }
      docPane.appendChild(inner);
      bodyDiv.appendChild(docPane);

      // ---- Pane 2: the decisions, sticky beside the reading. ----
      const rail = h('div', { className: 'srd-rail' });
      if (questions.length === 0) {
        rail.appendChild(
          h(
            'div',
            { className: 'srd-empty' },
            'No open questions in this doc — it is ready to mark reviewed.',
          ),
        );
        bodyDiv.appendChild(rail);
        return;
      }
      rail.appendChild(
        h('div', { className: 'srd-rail-title' }, `Decisions (${questions.length})`),
      );
      questions.forEach((q) => {
        const addressed = resolutionFor(q.anchor).trim().length > 0;
        const ta = h('textarea', {
          rows: 4,
          placeholder: 'Record the decision, deferral, or rationale…',
          disabled: isDone,
          value: resolutionFor(q.anchor),
        });
        const card = h(
          'div',
          { className: `srd-q ${addressed ? 'is-addressed' : ''}` },
          h(
            'div',
            { className: 'srd-q-head' },
            h('span', { className: 'srd-anchor' }, q.anchor),
            h('span', { className: 'srd-q-title' }, q.title),
          ),
          (() => {
            if (q.body_html) {
              const b = h('div', { className: 'srd-q-body' });
              b.innerHTML = q.body_html;
              return b;
            }
            return q.body_md ? h('div', { className: 'srd-q-body' }, q.body_md) : null;
          })(),
          h('label', { className: 'srd-label' }, 'Resolution'),
          ta,
        );
        // Toggle the addressed accent live, without re-rendering the
        // rail — a full re-render would blur the textarea mid-sentence.
        ta.addEventListener('input', (e) => {
          setResolution(q.anchor, e.target.value);
          card.classList.toggle('is-addressed', e.target.value.trim().length > 0);
        });
        rail.appendChild(card);
      });
      bodyDiv.appendChild(rail);
    }

    function renderActions() {
      actionsDiv.replaceChildren();
      if (saveError) {
        actionsDiv.appendChild(
          h(
            'div',
            { className: 'srd-error' },
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

    injectStyles();
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
