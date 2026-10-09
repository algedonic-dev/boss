// incident-review.js — custom Step UX for the `incident` Workflow's
// "Human review of the findings" step. Written for incident-post-mortem,
// whose review never mounted it; that protocol was folded into
// `incident` on 2026-09-24 (backlog 59d15039), whose review step is the
// first to declare kind=incident-review.
//
// WHY THIS EXISTS. Two feedback packets said the review step renders
// the findings unusably; one asked for "a custom step UX that
// presented the findings that I needed to sign-off on". The failure is
// structural, the same shape correction-verdict.js fixed: everything
// the reviewer needs to judge lives elsewhere — the Job's own metadata
// (summary, evidence, mitigations, open questions) and the answers the
// earlier steps recorded (timeline, attribution, detection,
// simplification, actions) — all of it behind a packet modal and a
// metadata dump. The question was on screen; the material to answer it
// was not.
//
// So this surface renders the whole post-mortem as ONE readable
// document — the Job's semi-structured metadata as ordered sections,
// then what each step found, labeled by its fields — and closes with
// the completion the step requires. Nothing here is new information;
// it is the same data, arranged to be read.
//
// THE RENDERER IS SEMI-STRUCTURED, deliberately. The two live packets
// already carry different metadata shapes. Keys the platform knows get
// first-class sections in reading order; every other key renders as a
// labeled prose block. Content is never dropped and never dumped as
// raw JSON. `sectionsFor` below is a deliberate near-copy of
// apps/web/src/it/incidents/postMortemDoc.ts — plugins are standalone
// JS bundles by design (no imports from the SPA), so the ordering is
// duplicated. Change one, change both.
//
// WHY A NEW KIND AND NOT A PLUGIN ON `task`. Plugins register by step
// kind and the SPA mounts by kind; registering for `task` would hijack
// every task step in the system (same rationale recorded on
// 146-correction-verdict-plugin.sql). The Workflow's review step
// declares kind=incident-review; the Rust StepRegistry does not need to
// learn the kind (validate_metadata is permissive for kinds it does not
// know).
//
// THE REVIEW COMPLETES ON THE REVIEWER'S PASSKEY (item 570c66e9,
// 2026-10-07: "human-only step completion should use passkey for
// enforcement"). The incident `review` step is `human_only` and names no
// sign-off role, so the jobs API completes it only on a verifying ticket
// from the person sending the completion — and this surface's completion
// was one bare PUT. It now draws what the passkey signs (the step's title
// and every metadata key, as the bytes they are), completes bare, and
// answers the server's 422 {required: "presence"} with ONE tap on the
// step as drawn and ONE retry. The ceremony itself is not here: it is
// passkey-ceremony.js beside this file, the one copy sign-off.js uses
// too, loaded by `withPasskey` at the foot before the mount is
// registered. This surface records no metadata key of its own, so it
// writes nothing before the tap: what is drawn is what is signed.
//
// Plugin contract: window.__boss_register_step_plugin(kind, mount).
// Host calls mount(container, props) with { step, jobId, onUpdate }.

(function () {
  // ---------------------------------------------------------------
  // Self-contained styling, injected once, scoped under
  // `.step-incident-review`, written against core's CSS custom
  // properties so it follows the tenant's theme rather than fighting
  // it. Same reasoning as review-design.js / correction-verdict.js.
  // ---------------------------------------------------------------
  const STYLE_ID = 'boss-incident-review-styles';
  const STYLES = `
.step-incident-review { max-width: 78ch; }

.step-incident-review .sir-head {
  display: flex; align-items: baseline; gap: 12px; flex-wrap: wrap;
  padding-bottom: 10px; margin-bottom: 18px;
  border-bottom: 1px solid var(--border, var(--hairline));
}
.step-incident-review .sir-head h3 { margin: 0; font-size: 17px; flex: 1 1 auto; }
.step-incident-review .sir-when {
  font-size: 12px; color: var(--text-dim, var(--static)); white-space: nowrap;
}

.step-incident-review .sir-section { margin: 0 0 14px; }
.step-incident-review .sir-label {
  margin: 0 0 3px; font-size: 11px; font-weight: 600;
  letter-spacing: 0.06em; text-transform: uppercase;
  color: var(--text-dim, var(--static));
}
.step-incident-review .sir-body {
  margin: 0; font-size: 13.5px; line-height: 1.6;
  white-space: pre-wrap; word-break: break-word;
  color: var(--text, var(--fog));
}

.step-incident-review .sir-steps-title {
  margin: 22px 0 10px; padding-top: 12px; font-size: 14px;
  border-top: 1px solid var(--border, var(--hairline));
}
.step-incident-review .sir-step {
  border: 1px solid var(--border, var(--hairline));
  border-left: 3px solid var(--accent, var(--signal));
  padding: 10px 14px; margin: 0 0 10px;
}
.step-incident-review .sir-step.sir-skipped {
  border-left-color: var(--border, var(--hairline));
}
.step-incident-review .sir-step h4 {
  margin: 0 0 8px; font-size: 13px;
}
.step-incident-review .sir-step .sir-by {
  font-weight: 400; font-size: 11px; color: var(--text-dim, var(--static));
}

.step-incident-review .sir-empty {
  font-size: 13px; font-style: italic; color: var(--text-dim, var(--static));
}
.step-incident-review .sir-err {
  font-size: 12px; color: var(--err, currentColor);
}

.step-incident-review .sir-actions {
  display: flex; align-items: center; gap: 12px;
  margin-top: 18px; padding-top: 12px;
  border-top: 1px solid var(--border, var(--hairline));
}
.step-incident-review .sir-actions button {
  font: inherit; font-size: 13px; font-weight: 600;
  padding: 7px 18px; cursor: pointer;
  border: 1px solid var(--accent, var(--signal));
  background: transparent; color: var(--accent, var(--signal));
}
.step-incident-review .sir-actions button:hover:not([disabled]) {
  background: var(--accent, var(--signal));
  color: var(--bg, var(--void));
}
.step-incident-review .sir-actions button[disabled] { opacity: 0.5; cursor: default; }
.step-incident-review .sir-done {
  font-size: 13px; padding: 10px 14px; margin-top: 18px;
  border: 1px solid var(--border, var(--hairline));
  color: var(--text-dim, var(--static));
}
`;

  function injectStyles() {
    if (document.getElementById(STYLE_ID)) return;
    const el = document.createElement('style');
    el.id = STYLE_ID;
    el.textContent = STYLES;
    document.head.appendChild(el);
  }

  // The document is built as nodes, never as an HTML string (backlog
  // 4a359b51, 2026-09-27): every finding, title and holder is metadata
  // any actor may write, and a string of markup is one missed escape
  // from running it. A string child is a Text node by append's
  // definition.
  function h(tag, attrs, ...children) {
    const el = document.createElement(tag);
    if (attrs) {
      for (const k in attrs) {
        const v = attrs[k];
        if (v == null || v === false) continue;
        if (k === 'className') el.className = v;
        else if (k.startsWith('on') && typeof v === 'function') {
          el.addEventListener(k.slice(2).toLowerCase(), v);
        } else if (k === 'disabled') {
          el[k] = v;
        } else {
          el.setAttribute(k, String(v));
        }
      }
    }
    for (const child of children.flat()) {
      if (child == null || child === false) continue;
      el.append(child instanceof Node ? child : String(child));
    }
    return el;
  }

  function humanize(key) {
    const spaced = String(key).replace(/[_-]+/g, ' ').trim();
    return spaced.charAt(0).toUpperCase() + spaced.slice(1);
  }

  // Flatten a metadata value to prose. Mirrors postMortemDoc.ts:
  // strings pass through, scalars stringify, arrays become lines, flat
  // objects become labeled lines. null = nothing to render.
  function prose(value) {
    if (value === null || value === undefined) return null;
    if (typeof value === 'string') return value.trim() === '' ? null : value;
    if (typeof value === 'number' || typeof value === 'boolean') return String(value);
    if (Array.isArray(value)) {
      if (value.length === 0) return null;
      return value.map((v) => (typeof v === 'string' ? v : JSON.stringify(v))).join('\n');
    }
    if (typeof value === 'object') {
      const entries = Object.entries(value);
      if (entries.length === 0) return null;
      return entries
        .map(([k, v]) => humanize(k) + ': ' + (typeof v === 'string' ? v : JSON.stringify(v)))
        .join('\n');
    }
    return String(value);
  }

  // Reading order + labels for the keys the platform knows — the
  // deliberate near-copy of postMortemDoc.ts (see the header comment).
  const KNOWN_LABELS = {
    incident_at: 'When it happened',
    incident_date: 'When it happened',
    summary: 'Summary',
    timeline: 'Timeline',
    root_cause: 'Root cause',
    open_questions: 'Open questions',
    evidence: 'Evidence',
  };
  const READING_ORDER = [
    'incident_at', 'incident_date', 'summary', 'timeline', 'root_cause',
    '#mitigations', 'open_questions', 'evidence',
  ];

  function sectionsFor(metadata, omit) {
    const skip = new Set(omit || []);
    const mitigationsSlot = READING_ORDER.indexOf('#mitigations');
    const rank = (key) => {
      const slot = READING_ORDER.indexOf(key);
      if (slot >= 0) return slot;
      if (key.indexOf('mitigation') === 0) return mitigationsSlot;
      return READING_ORDER.length;
    };
    return Object.keys(metadata || {})
      .filter((key) => !skip.has(key))
      .map((key, authored) => ({ key, authored }))
      .sort((a, b) => rank(a.key) - rank(b.key) || a.authored - b.authored)
      .map(({ key }) => ({
        key,
        label: KNOWN_LABELS[key] || humanize(key),
        body: prose(metadata[key]),
      }))
      .filter((s) => s.body !== null);
  }

  // Plumbing keys that are not findings: authority gating and trigger
  // bookkeeping, plus agent hand-off records.
  const PLUMBING = new Set([
    'authority_role', 'trigger_kind', 'trigger_name',
    'agent_requested_at', 'agent_requested_by',
  ]);

  function sectionNode(s) {
    return h(
      'section',
      { className: 'sir-section' },
      h('h5', { className: 'sir-label' }, s.label),
      h('p', { className: 'sir-body' }, s.body),
    );
  }

  function mount(container, props) {
    injectStyles();
    const step = props.step;
    const jobId = props.jobId;
    const onUpdate = props.onUpdate;

    let job = null;
    let loadError = null;
    let saving = false;
    let saveError = null;
    // What the passkey signs, as last drawn: {title, metadata} COPIED at
    // the render, so a later change to the step this mount was handed is
    // not mistaken for what is on screen. null while no block is drawn.
    let onScreen = null;
    // Set when the server asked a passkey of a step that declares neither
    // human_only nor presence (a kind's floor): the block is drawn then.
    let revealed = false;
    // Set by the cleanup. An unmounted surface shows nothing, so a
    // ceremony still in flight refuses, and a prompt still up is aborted.
    let disposed = false;
    const unmounted = new AbortController();
    let overflowObservers = [];

    const root = document.createElement('div');
    root.className = 'step-surface step-incident-review';
    container.appendChild(root);

    const terminal = step.status === 'completed' || step.status === 'skipped';

    // The declaration, read the way the server reads it (boss-jobs
    // human_only::declared): the bool `true` or the string "true".
    const humanOnly = () => {
      const v = (step.metadata || {}).human_only;
      return v === true || (typeof v === 'string' && v.trim().toLowerCase() === 'true');
    };
    const signedVisible = () =>
      !disposed && !terminal && (humanOnly() || step.assurance_required === 'presence' || revealed);

    // WHAT THE PASSKEY SIGNS, DRAWN (design f623e425 D3): the title and
    // EVERY metadata key the shape hash covers, each as the bytes it is
    // (P.signedText), a box that scrolls saying so (6093cf13). Derived
    // from the step's own keys, never an allow-list, so it cannot fall
    // behind the hash.
    function signedBlock() {
      overflowObservers.forEach((o) => o.disconnect());
      overflowObservers = [];
      onScreen = null;
      if (!signedVisible()) return null;
      const md = step.metadata || {};
      onScreen = { title: step.title, metadata: JSON.parse(JSON.stringify(md)) };
      const rows = Object.keys(md)
        .sort()
        .map((k) => {
          const text = P.signedText(md[k]);
          const pre = h('pre', { className: 'step-signed-value' }, text);
          const note = h('div', { className: 'step-signed-overflow' });
          const check = () => {
            const scrolls =
              pre.scrollHeight > pre.clientHeight + 1 || pre.scrollWidth > pre.clientWidth + 1;
            note.textContent = scrolls ? P.scrollNote(text) : '';
          };
          if (typeof ResizeObserver === 'function') {
            const o = new ResizeObserver(check);
            o.observe(pre);
            overflowObservers.push(o);
          }
          // Measured once it is laid out.
          Promise.resolve().then(check);
          return h(
            'div',
            { className: 'step-signed-row' },
            h('div', { className: 'step-signed-key' }, P.signedText(k)),
            pre,
            note,
          );
        });
      return h(
        'section',
        { className: 'step-signed-keys', 'aria-label': 'What your passkey signs' },
        h('div', { className: 'step-signed-keys-head' }, 'What your passkey signs'),
        h(
          'p',
          { className: 'step-signed-keys-note' },
          'Completing this review takes your passkey. It signs the step ',
          h('strong', { className: 'step-signed-title' }, P.signedText(step.title)),
          ' and every key below, exactly as shown. Text in double quotes has each character you could not otherwise see or tell apart written as an escape.',
        ),
        rows,
      );
    }

    function findings() {
      if (loadError) {
        // The findings are the point — say so rather than rendering a
        // confident-looking empty review (false-empty sweep).
        return [
          h(
            'p',
            { className: 'sir-err' },
            `Could not load the post-mortem findings — ${loadError}. Reload before reviewing.`,
          ),
        ];
      }
      if (!job) return [h('p', { className: 'sir-empty' }, 'Loading the findings…')];

      const meta = job.metadata || {};
      const when = prose(meta.incident_at) || prose(meta.incident_date);
      const sections = sectionsFor(meta, ['incident_at', 'incident_date']);

      // What each sibling step found: terminal steps other than this
      // one, in workflow order, each field labeled. A step whose
      // metadata is all plumbing recorded no findings and is omitted.
      const siblings = (Array.isArray(job.steps) ? job.steps : [])
        .filter((s) => s.id !== step.id)
        .filter((s) => s.status === 'completed' || s.status === 'skipped')
        .sort((a, b) => (a.sort_order || 0) - (b.sort_order || 0))
        .map((s) => ({
          step: s,
          found: sectionsFor(s.metadata || {}, [...PLUMBING]),
        }))
        .filter((x) => x.found.length > 0);

      return [
        h(
          'div',
          { className: 'sir-head' },
          h('h3', null, job.title || 'Post-mortem findings'),
          when ? h('span', { className: 'sir-when' }, when) : null,
        ),
        sections.length
          ? sections.map(sectionNode)
          : h('p', { className: 'sir-empty' }, 'The packet carries no findings in its metadata yet.'),
        siblings.length ? h('h4', { className: 'sir-steps-title' }, 'What each step found') : null,
        siblings.map((x) =>
          h(
            'article',
            { className: `sir-step${x.step.status === 'skipped' ? ' sir-skipped' : ''}` },
            h(
              'h4',
              null,
              x.step.title,
              x.step.assignee_id
                ? [' ', h('span', { className: 'sir-by' }, `· ${x.step.assignee_id}`)]
                : null,
            ),
            x.found.map(sectionNode),
          ),
        ),
      ].flat();
    }

    function actions() {
      if (terminal) {
        return h(
          'div',
          { className: 'sir-done' },
          `Review recorded — this step is ${step.status}. The document above is the durable record.`,
        );
      }
      // A findings load failure must not offer completion: reviewing a
      // post-mortem nobody has seen is not a review (same gate
      // review-design.js carries).
      if (loadError || !job) return null;
      return h(
        'div',
        { className: 'sir-actions' },
        h(
          'button',
          { type: 'button', 'data-action': 'complete', disabled: saving, onClick: complete },
          saving ? 'Recording…' : 'Findings reviewed — complete review',
        ),
        saveError ? h('span', { className: 'sir-err' }, saveError) : null,
      );
    }

    function render() {
      root.replaceChildren(...[...findings(), signedBlock(), actions()].filter(Boolean));
    }

    // The ceremony over the step AS DRAWN (passkey-ceremony.js). A key
    // the passkey would sign that the block did not draw — the step this
    // mount holds changed after the draw, or the block was not up —
    // refuses before any request; the block is drawn as the step now
    // stands, and the reviewer reads it and presses again.
    //
    // THE BEGIN NAMES WHAT THIS SURFACE SHOWED, and it is not a write:
    // assert/begin stores nothing on the step, it only compares, and the
    // gateway refuses (412) a shown step that is not the step as it
    // stands. So the lost update step-plugins-own-their-keys refuses
    // cannot happen here; the snapshot is named for what it is, as
    // sign-off.js names it.
    function passkeyTicket() {
      const renderedMetadata = step.metadata || {};
      return P.ticket({
        jobId,
        stepId: step.id,
        shown: { title: step.title, metadata: renderedMetadata },
        onScreen: () => onScreen,
        isGone: () => disposed,
        signal: unmounted.signal,
        onUnseen: (unseen) => {
          revealed = true;
          return `nothing was signed: your passkey would sign ${unseen.join(', ')}, which this page had not shown — it is shown now; read it and press again`;
        },
      });
    }

    // The two writes this surface makes, each spelled in full (the pin
    // a-step-plugin-put-carries-no-metadata reads the completion's body
    // off this text). Each may be refused for presence: it is sent bare,
    // then — on a 422 {required: "presence"} — once more carrying the
    // ticket of ONE ceremony.
    const withTicket = (ticket) => {
      const headers = { 'Content-Type': 'application/json' };
      if (ticket) headers['x-presence-ticket'] = ticket;
      return headers;
    };
    const signOff = (role) => (ticket) =>
      fetch(`/api/jobs/${jobId}/steps/${step.id}/sign-offs`, {
        method: 'POST',
        headers: withTicket(ticket),
        body: JSON.stringify({ role }),
      });
    const completeStep = (ticket) =>
      fetch(`/api/jobs/${jobId}/steps/${step.id}`, {
        method: 'PUT',
        headers: withTicket(ticket),
        body: JSON.stringify({ status: 'completed' }),
      });

    async function complete() {
      if (saving) return;
      // WHAT IS SIGNED IS WHAT WAS READ. Every render below redraws the
      // signing block from the step as it then stands, so the check
      // comes FIRST, against the block as the reviewer saw it: a key
      // that changed on this step since that draw refuses here, before
      // any request and before the passkey is asked, and the block is
      // drawn again for them to read.
      if (onScreen) {
        const renderedMetadata = step.metadata || {};
        const unseen = P.notShown({ title: step.title, metadata: renderedMetadata }, onScreen);
        if (unseen.length > 0) {
          saveError = `Nothing was signed or sent: ${unseen.join(', ')} changed on this step after this page drew it — it is drawn again now; read it and press again.`;
          render();
          return;
        }
      }
      saving = true;
      saveError = null;
      render();
      try {
        // This surface records no metadata keys of its own, so it
        // writes no metadata at all: the completion is the status
        // alone (backlog e39a9d2a, design 93d2bddb). It first re-sent
        // the page-load snapshot, a lost update that reverted any key
        // recorded after this page loaded, then read the row back and
        // PUT that whole — correct, but a metadata body on the step
        // PUT, and that PUT is closing to any metadata body. The PUT
        // keeps every field a body omits.
        //
        // Stamp every required sign-off role first, in the step's
        // final shape (v1 of the workflow requires none; this stays
        // generic so a v2 that adds one keeps working).
        //
        // A stamp or the completion refused {required: "presence"} is
        // answered with ONE passkey tap on the step as drawn and ONE
        // retry (P.completeOnce). The v1 step is human_only with no
        // role, so the tap comes on the completion; a row that names a
        // role is stamped on the tap and then completes bare.
        const tapped = async (what, put) => {
          const answer = await P.completeOnce({ put, ticket: passkeyTicket });
          if (answer.failed) {
            const e = answer.failed;
            const why = e && e.message ? e.message : String(e);
            const refused = new Error(
              `${what} needs your passkey, and it was not given — ${why}. The step is still open; nothing was completed.`,
            );
            refused.moved = Boolean(e && e.status === 412);
            throw refused;
          }
          if (!answer.done.ok) {
            const again = answer.retried && (await P.refusedForPresence(answer.done));
            const text = await answer.done.text();
            throw new Error(
              again
                ? `${what} was refused again after a fresh passkey tap — HTTP ${answer.done.status}: ${text}. The step is still open.`
                : `${what} failed (HTTP ${answer.done.status}): ${text}`,
            );
          }
        };
        for (const role of step.sign_offs_required || []) {
          await tapped(`The sign-off as ${role}`, signOff(role));
        }
        // Read the code — a swallowed non-2xx leaves the surface
        // looking saved while the packet never moved.
        await tapped('Completing the review', completeStep);
        onUpdate();
      } catch (e) {
        saveError = e && e.message ? e.message : String(e);
        saving = false;
        render();
        // The step moved under this surface (the gateway's 412): what is
        // drawn is no longer what the step holds, so the host re-reads.
        if (e && e.moved && typeof onUpdate === 'function') onUpdate();
      }
    }

    // First paint immediately so the surface is never blank; the
    // document fills in when the Job lands. One fetch: the list
    // endpoint already enriches the Job with its steps.
    render();
    fetch(`/api/jobs/${jobId}`)
      .then((r) => {
        if (!r.ok) throw new Error(`HTTP ${r.status}`);
        return r.json();
      })
      .then((j) => {
        job = j;
        render();
      })
      .catch((e) => {
        loadError = e && e.message ? e.message : String(e);
        render();
      });

    return function cleanup() {
      disposed = true;
      unmounted.abort();
      overflowObservers.forEach((o) => o.disconnect());
      overflowObservers = [];
      root.remove();
    };
  }

  // passkey-ceremony.js, bound before the mount is registered (see
  // `withPasskey`): the one copy of the ceremony, shared with sign-off.js.
  let P;

  // The bundle registers its mount only once passkey-ceremony.js has
  // run. It is a classic script, so it cannot import; it adds the shared
  // file's script tag itself, from the directory it was served from, and
  // the host — which already waits for a registration — knows nothing of
  // it. A shared file that does not load registers a mount that says so,
  // rather than leaving the step blank. (The same dozen lines stand at
  // the foot of sign-off.js: they are what loads the shared file, so
  // they cannot live in it.)
  function withPasskey(ready, failed) {
    if (window.__boss_passkey) return ready(window.__boss_passkey);
    const script = document.createElement('script');
    script.src = '/plugins/passkey-ceremony.js';
    script.onload = () =>
      window.__boss_passkey ? ready(window.__boss_passkey) : failed('it ran and defined nothing');
    script.onerror = () => failed('it did not load');
    document.head.appendChild(script);
  }

  if (typeof window.__boss_register_step_plugin !== 'function') {
    console.error('[incident-review-plugin] __boss_register_step_plugin not on window');
    return;
  }
  withPasskey(
    (shared) => {
      P = shared;
      window.__boss_register_step_plugin('incident-review', mount);
    },
    (why) => {
      window.__boss_register_step_plugin('incident-review', (container) => {
        const p = document.createElement('p');
        p.className = 'sir-err';
        p.textContent = `This review surface cannot run: /plugins/passkey-ceremony.js — ${why}. The review cannot be completed here; reload the page.`;
        container.append(p);
      });
    },
  );
})();
