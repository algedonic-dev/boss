// sign-off.js v3 — the surface for a step that needs someone's name on
// a DECISION, showing them what they are deciding.
//
// v1 (car 884b85f4's train) rendered the stamp ceremony — the role
// roster, who signed when, stale-stamp 409s — and nothing else. That
// solved "Missing custom step UX" (b1aa1f5f) for ceremony steps and
// then, because a plugin evicts the platform surface for its whole
// KIND, blinded every decision-shaped sign-off in the system:
// 19db52de, David on his publish approval, "There is just a sign and
// complete button, which doesn't seem like much of a choice." The row
// was retired live on 2026-08-19; this version earns it back.
//
// What a sign-off step actually carries, all now rendered:
//   1. THE CASE — step.metadata.context_md, else the job's context_md,
//      else the job's filed message (the DecisionContext chain; a
//      plugin is exempt from the host panel, so it folds in here).
//   2. THE CONTRACT — step.fields declares required-at-done metadata
//      (publish-to-github v3 requires `approved`); v1 offered a
//      Complete that could only 400 against these, and did not show
//      the 400. Declared fields render as inputs, enums as selects,
//      and the decision buttons stay disabled until required ones are
//      filled — a button that exists only to produce an error teaches
//      the operator to distrust buttons (v1's own line, kept).
//   3. THE DECISION — Approve / Reject / Request changes, writing the
//      decision trio (`decision`, `decided_at`, `comment`) exactly as
//      the platform ApprovalSurface does, so protocol predicates read
//      one vocabulary regardless of which surface recorded it.
//   4. THE CEREMONY — v1's roster, verbatim in behavior: stamps are
//      collected per role, the step cannot complete while one is
//      outstanding, and a 409 surfaces the server's stale-roles text.
//   5. WHAT THE PASSKEY SIGNS — on a presence step, the title and EVERY
//      metadata key, drawn from the step's own keys (design f623e425
//      D3; backlog 6c9183de extends b and c, 2026-09-25). The passkey
//      binds step_shape_hash(title, metadata), and this surface drew
//      only the declared fields, so an ops-request's verb, host, args,
//      rendered_plan_sha256 or a planted decision was signed by the
//      per-role button unseen. A ceremony on a step whose block is not
//      drawn — a kind whose floor demands presence the step never
//      declared — signs nothing: it draws the block and asks for the
//      tap again. Title, key names and values are drawn AS THEIR BYTES —
//      quoted and escaped wherever a reader could otherwise misread them
//      (backlog 6093cf13) — a box that scrolls says so, and an unmounted
//      surface signs nothing.
//
// Order on Approve/Reject (v3, feedback 221b4b5c): metadata lands
// first (a stamp attests the step's current shape, so the decision
// must be IN the shape), then the user's own stamp if their role is
// required and unsigned, then the completion — skipped, with a plain
// explanation, while other roles' signatures are still outstanding.
// On a presence step the passkey is asked for the STAMP; the completion
// is sent bare and stands on the stamps (design 1ce67f7e), and no ticket
// is kept between the two.
// Request changes records without completing — unless the step's
// protocol declares `changes_requested_completes = true`, when it takes
// the Approve path (backlog da322e8f). NOTHING writes metadata
// after a signature exists: on 2026-09-05 15:40 David signed, then
// this surface re-saved his unchanged decision with a fresh
// decided_at, and the completion answered 409 stale two seconds after
// his signature. A decision that a signature already covers is not
// re-saved; one that changes is saved BEFORE the (re-)signature, and
// the roster says which stamps that made stale. Each stage of the
// gesture is shown as it lands — saved, signed, completed — and any
// refusal verbatim, so a tap never looks like it did nothing.
//
// Plugin contract: window.__boss_register_step_plugin(kind, mount);
// mount(container, { step, jobId, onUpdate, currentUser }).

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
    // append, never appendChild(child-or-string): a string handed to
    // append is a Text node by definition, so a title, a metadata value
    // or a refusal is drawn as its characters. CodeQL read the old
    // ternary's raw arm as HTML and failed publish PR #245 on this line
    // (backlog 4a359b51); stepPluginHtmlSinks.test.ts holds the shape.
    for (const child of children.flat()) {
      if (child == null || child === false) continue;
      el.append(child instanceof Node ? child : String(child));
    }
    return el;
  }

  function when(iso) {
    if (!iso) return '';
    const d = new Date(iso);
    return Number.isNaN(d.getTime()) ? String(iso) : d.toLocaleString();
  }

  // The decision trio is the step KIND's own vocabulary (seeded with
  // the kind when it absorbed the retired `approval` kind); declared
  // per-workflow fields are anything beyond it.
  const TRIO = ['decision', 'decided_at', 'comment'];

  function nonEmptyString(v) {
    return typeof v === 'string' && v.trim().length > 0 ? v : null;
  }

  // HOW WHAT A PASSKEY SIGNS IS DRAWN, AND THE CEREMONY ITSELF, live in
  // passkey-ceremony.js beside this file — one copy, shared with every
  // other bundle that asks a passkey (incident-review.js, since a
  // human-only step completes on one: item 570c66e9, 2026-10-07). They
  // were defined here until then. Bound by `withPasskey` at the foot of
  // this file BEFORE the mount is registered, so every function below
  // finds them set. signedText draws a value as the bytes it is
  // (6093cf13); scrollNote is what a box that scrolls says; canonical
  // and notShown are the check that what would be signed was drawn. The
  // app's copies are in apps/web/src/steps/presence.ts, and
  // signOffPlugin.test.ts pins the two equal on generated inputs.
  let P;
  let signedText;
  let scrollNote;
  let canonical;
  let notShown;

  function mount(container, { step, jobId, onUpdate }) {
    const required = Array.isArray(step.sign_offs_required) ? step.sign_offs_required : [];
    let stamps = Array.isArray(step.sign_offs) ? step.sign_offs.slice() : [];
    const isDone = step.status === 'completed' || step.status === 'skipped';
    let busy = false;
    let error = null;
    // Roles whose recorded stamp no longer matches the step's shape —
    // the server's rule (a stamp pins step_shape_hash; a completion-
    // relevant write after it records STEP_STAMPS_INVALIDATED and the
    // stamp stays for provenance). Tracked here so the roster tells
    // the truth between a change and the next read of the step.
    const stale = new Set();
    // The stages of the current gesture, in the order they landed.
    let progress = [];
    // NO TICKET IS KEPT BETWEEN REQUESTS (design 1ce67f7e, 2026-10-07).
    // A ceremony's ticket rides the one request it was run for and is
    // gone. This surface used to hold the stamp's ticket to send it
    // again with the completion (b568044a): the jobs API judged a
    // completion on that request's own header alone, so a bare PUT after
    // a presence stamp answered 422. A step that names sign-off roles now
    // completes on the live passkey stamps it holds, so the completion
    // carries nothing — which also ends the two-minute race and the
    // failure when the stamp and the completion happened on different
    // surfaces.
    // What the passkey signs, as last drawn: {title, metadata} copied at
    // the render, so a later write to the local cache is not mistaken for
    // what is on screen. null while no signed content is drawn.
    let onScreen = null;
    // Set when a ceremony was asked of a step that never declared
    // presence: from then on its signed content is drawn too.
    let revealed = false;
    // Set by the mount's cleanup (backlog 6093cf13). The host unmounts
    // this surface when the rail moves to another step, and a gesture
    // already running kept going: it drew into the detached tree, its
    // copy of "what is on screen" still matched, and the passkey prompt
    // came up over the NEXT step to sign this one. Unmounted, nothing of
    // this step is on screen, so nothing is drawn as signed and any
    // ceremony still in flight refuses.
    let disposed = false;
    // Aborted by the same cleanup, so a passkey prompt still up when the
    // rail moves on comes down with the surface rather than waiting over
    // the next step (backlog 7c53b1bf, review of car fcda5f8b).
    const unmounted = new AbortController();
    const signedVisible = () =>
      !disposed && !isDone && (step.assurance_required === 'presence' || revealed);
    // The overflow checks of the rows as last drawn, and the observers
    // that re-run them when a box changes size.
    let overflowChecks = [];
    let overflowObservers = [];

    const declared = (Array.isArray(step.fields) ? step.fields : []).filter(
      (f) => f && f.name && !TRIO.includes(f.name),
    );
    // Live values for declared fields, seeded from step metadata (a
    // pre-filled `approved` renders filled and editable). A value that is
    // not a string is seeded as its JSON (backlog 6c9183de, review S4):
    // String([]) is '', so ops-request's required `args` on a zero-arg
    // request read as an empty required input and Approve never opened.
    // Present is present, whatever its JSON type.
    const fieldValues = {};
    declared.forEach((f) => {
      const cur = (step.metadata || {})[f.name];
      fieldValues[f.name] =
        cur == null ? '' : typeof cur === 'string' ? cur : JSON.stringify(cur);
    });
    // WHAT A PASSKEY SIGNS IS SHOWN, NOT OFFERED FOR EDIT (adversarial
    // re-review of fd7090cc, 2026-09-25). On a presence-assured step a
    // declared field that already holds a value is the document the
    // signature binds — an ops-request's `plan`, rendered on the host.
    // As a one-line text input it lost its newlines on screen, so the
    // approver read a flattened plan, and could edit it under the
    // signature. It renders read-only in a <pre>, byte for byte, and the
    // decision patch never re-writes it: this surface did not author it.
    const signedDoc = new Set(
      step.assurance_required === 'presence'
        ? declared.filter((f) => nonEmptyString(fieldValues[f.name])).map((f) => f.name)
        : [],
    );

    // Whether Request changes COMPLETES this step, read off the step's
    // own metadata — the protocol's declaration, not this surface's
    // guess (backlog da322e8f, 2026-09-23). An ordinary sign-off keeps
    // its step open on changes-requested so the same approver can
    // re-decide once the thing is revised. A protocol that ROUTES the
    // decision — page-audit's `revise` is ready_when `steps.review.done
    // AND decision = "changes-requested"` — needs the step done, and
    // declares it with `changes_requested_completes = true` in the
    // step's metadata_defaults. Undeclared, the old behaviour stands:
    // three founder change requests sat recorded and unrouted until the
    // operator completed them by hand, which is what this ends.
    const changesRequestedCompletes = (step.metadata || {}).changes_requested_completes === true;

    const stampFor = (role) => stamps.find((s) => s && s.role === role);
    const outstanding = () => required.filter((r) => !stampFor(r) || stale.has(r));
    const missingRequired = () =>
      declared.filter((f) => f.required && !nonEmptyString(fieldValues[f.name]));

    const contextDiv = h('div', { className: 'step-signoff-context' });
    const fieldsDiv = h('div', { className: 'step-signoff-fields' });
    const signedDiv = h('div', { className: 'step-signed-keys' });
    const rolesDiv = h('div', { className: 'step-signoff-roles' });
    const actionsDiv = h('div', { className: 'step-actions' });
    const errorDiv = h('div', { className: 'step-signoff-error' });
    const progressDiv = h('div', { className: 'step-signoff-progress' });
    const commentTa = h('textarea', {
      className: 'step-signoff-comment',
      rows: '2',
      placeholder: 'Comment (optional)…',
    });
    commentTa.value = String((step.metadata || {}).comment || '');

    // `signed` is false for the packet's own text (its briefing or filed
    // message): that is job metadata, outside the step's shape hash, so a
    // passkey on this step does not sign it and it can change under a
    // signature without voiding it — and its links are drawn as their
    // text, not their targets. The card says so (6093cf13). The step's
    // own context_md IS one of the step's keys, drawn as its bytes in the
    // signed block, and carries no such label.
    function renderContext(text, sourceLabel, signed) {
      contextDiv.replaceChildren();
      if (!text) return;
      // The case renders as MARKDOWN when the host provides its
      // escape-first renderer (window.__boss_markdown, one definition
      // for every bundle — 2244db9e), and as preserved text when it
      // does not (older SPA, tests). The innerHTML is earned by the
      // renderer's contract: everything is escaped before any tag it
      // emits, hrefs are http(s)/relative only.
      const render = window.__boss_markdown;
      const body = h('div', { className: 'step-signoff-context-body' });
      if (typeof render === 'function') {
        body.innerHTML = render(text);
      } else {
        body.textContent = text;
      }
      contextDiv.appendChild(
        h(
          'div',
          { className: 'step-signoff-context-card' },
          h(
            'div',
            { className: 'step-signoff-context-head' },
            h('span', { className: 'step-signoff-context-title' }, 'What this decision is about'),
            h('span', { className: 'step-signoff-context-source' }, sourceLabel),
            signed
              ? null
              : h('span', { className: 'step-signoff-context-unsigned' }, 'not signed'),
          ),
          body,
        ),
      );
    }

    function renderFields() {
      fieldsDiv.replaceChildren();
      if (declared.length === 0) return;
      declared.forEach((f) => {
        const id = `signoff-field-${step.id}-${f.name}`;
        const type = String(f.field_type || 'string');
        // Drawn once: while the signed block is up it carries this field,
        // byte for byte, with every other key the passkey signs.
        if (signedDoc.has(f.name) && signedVisible()) return;
        if (signedDoc.has(f.name)) {
          // The step's own value, as the signed block draws it — never
          // `fieldValues`, the input copy, where an array is seeded as its
          // JSON text and so read as the string "[]" (follow-up b of the
          // review of car f3365343, 2026-09-28).
          fieldsDiv.appendChild(
            h(
              'div',
              { className: 'step-field' },
              h('label', { for: id }, `${f.name} — what your passkey signs`),
              h(
                'pre',
                { className: 'step-signoff-signed', id },
                signedText((step.metadata || {})[f.name]),
              ),
            ),
          );
          return;
        }
        let input;
        if (type.includes('|')) {
          input = h('select', { className: 'step-signoff-input', id });
          const opts = type.split('|').map((o) => o.trim()).filter(Boolean);
          if (!opts.includes(fieldValues[f.name])) {
            input.appendChild(h('option', { value: '' }, '— choose —'));
          }
          opts.forEach((o) => input.appendChild(h('option', { value: o }, o)));
          input.value = fieldValues[f.name];
        } else {
          input = h('input', { className: 'step-signoff-input', id, type: 'text' });
          input.value = fieldValues[f.name];
        }
        input.addEventListener('input', (e) => {
          fieldValues[f.name] = e.target.value;
          renderActions();
        });
        input.addEventListener('change', (e) => {
          fieldValues[f.name] = e.target.value;
          renderActions();
        });
        if (isDone) input.disabled = true;
        fieldsDiv.appendChild(
          h(
            'div',
            { className: 'step-field' },
            h('label', { for: id }, f.required ? `${f.name} (required)` : f.name),
            input,
          ),
        );
      });
    }

    // A value box that scrolls — its content taller or wider than the box
    // — carries a note under it saying how much there is (6093cf13). Run
    // at each draw, again once the root is attached (a detached box has no
    // size), and whenever a box changes size where the browser can say so.
    function watchOverflow(pre, note, text) {
      const check = () => {
        const scrolls =
          pre.scrollHeight > pre.clientHeight + 1 || pre.scrollWidth > pre.clientWidth + 1;
        note.textContent = scrolls ? scrollNote(text) : '';
      };
      overflowChecks.push(check);
      if (typeof ResizeObserver === 'function') {
        const observer = new ResizeObserver(check);
        observer.observe(pre);
        overflowObservers.push(observer);
      }
      check();
    }

    // Every key the passkey signs, from the step's own keys — never an
    // allow-list, so a key nobody wrote a renderer for is drawn as its
    // JSON rather than skipped — and the copy presenceTicket() compares
    // against is taken HERE, from what was just drawn. The title, each
    // key name and each value are drawn as their bytes (signedText).
    function renderSigned() {
      signedDiv.replaceChildren();
      overflowObservers.forEach((o) => o.disconnect());
      overflowObservers = [];
      overflowChecks = [];
      onScreen = null;
      if (!signedVisible()) return;
      const md = step.metadata || {};
      signedDiv.appendChild(
        h('div', { className: 'step-signed-keys-head' }, 'What your passkey signs'),
      );
      signedDiv.appendChild(
        h(
          'p',
          { className: 'step-signed-keys-note' },
          'The step ',
          h('strong', { className: 'step-signed-title' }, signedText(step.title)),
          ' and every key below, exactly as shown. Text in double quotes has each character you could not otherwise see or tell apart written as an escape. Your decision, its time and your comment join them when you press a button.',
        ),
      );
      Object.keys(md)
        .sort()
        .forEach((k) => {
          const text = signedText(md[k]);
          const pre = h('pre', { className: 'step-signed-value' }, text);
          const note = h('div', { className: 'step-signed-overflow' });
          signedDiv.appendChild(
            h(
              'div',
              { className: 'step-signed-row' },
              h('div', { className: 'step-signed-key' }, signedText(k)),
              pre,
              note,
            ),
          );
          watchOverflow(pre, note, text);
        });
      onScreen = { title: step.title, metadata: JSON.parse(JSON.stringify(md)) };
    }

    function renderRoles() {
      rolesDiv.replaceChildren();
      if (required.length === 0) {
        rolesDiv.appendChild(
          h(
            'p',
            { className: 'step-signoff-none' },
            'No counter-signatures are required — your decision completes the step.',
          ),
        );
        return;
      }
      required.forEach((role) => {
        const stamp = stampFor(role);
        const isStale = Boolean(stamp) && stale.has(role);
        const row = h(
          'div',
          {
            className: `step-signoff-role ${stamp && !isStale ? 'is-signed' : 'is-outstanding'}`,
          },
          h('span', { className: 'step-signoff-rolename' }, role),
          stamp
            ? h(
                'span',
                { className: isStale ? 'step-signoff-stale' : 'step-signoff-stamp' },
                `signed by ${stamp.authority_id || 'unknown'} · ${when(stamp.stamped_at)}${
                  isStale ? ' — stale: the step changed after signing; sign again' : ''
                }`,
              )
            : h('span', { className: 'step-signoff-await' }, 'awaiting signature'),
          (!stamp || isStale) && !isDone
            ? h(
                'button',
                { className: 'step-btn', disabled: busy, onClick: () => sign(role) },
                `Sign off as ${role}`,
              )
            : null,
        );
        rolesDiv.appendChild(row);
      });
    }

    function renderActions() {
      actionsDiv.replaceChildren();
      if (isDone) {
        const d = (step.metadata || {}).decision;
        if (d && d !== 'pending') {
          actionsDiv.appendChild(
            h('div', { className: `step-signoff-result step-signoff-${d}` }, `Decision: ${d}`),
          );
        }
        return;
      }
      const missing = missingRequired();
      if (missing.length > 0) {
        actionsDiv.appendChild(
          h(
            'span',
            { className: 'step-signoff-blocked' },
            `Fill the required field${missing.length === 1 ? '' : 's'} first: ${missing
              .map((f) => f.name)
              .join(', ')}`,
          ),
        );
      }
      const disabled = busy || missing.length > 0;
      actionsDiv.appendChild(
        h(
          'button',
          { className: 'step-btn step-btn-approve', disabled, onClick: () => decide('approved') },
          'Approve',
        ),
      );
      actionsDiv.appendChild(
        h(
          'button',
          { className: 'step-btn step-btn-reject', disabled, onClick: () => decide('rejected') },
          'Reject',
        ),
      );
      actionsDiv.appendChild(
        h(
          'button',
          {
            className: 'step-btn',
            // A Request changes that completes meets the same
            // required-at-done contract as Approve, so it waits too.
            disabled: changesRequestedCompletes ? disabled : busy,
            onClick: () => decide('changes-requested'),
          },
          'Request changes',
        ),
      );
    }

    function renderError() {
      errorDiv.replaceChildren();
      if (!error) return;
      errorDiv.appendChild(h('p', { className: 'step-error' }, error));
    }

    function renderProgress() {
      progressDiv.replaceChildren();
      if (progress.length === 0) return;
      progressDiv.appendChild(
        h('p', { className: 'step-signoff-stages' }, progress.join(' · ')),
      );
    }

    function renderAll() {
      renderFields();
      renderSigned();
      renderRoles();
      renderActions();
      renderProgress();
      renderError();
    }

    // Presence ceremony (docs/design/presence.md): a presence-gated
    // step refuses a plain stamp with 422 {required:"presence"}; the
    // passkey then signs a challenge bound to this step's CURRENT
    // shape hash and the stamp retries with the issued ticket. No
    // fallback path (Q3). The ceremony is P.ticket (passkey-ceremony.js):
    // this surface hands it what it owns — the step as rendered, with
    // this gesture's own decision folded in by decide() below, never a
    // fresh read (fd7090cc); what its signed block drew; whether it is
    // still mounted (6093cf13); and the signal its cleanup aborts
    // (7c53b1bf).
    function presenceTicket() {
      const renderedMetadata = step.metadata || {};
      return P.ticket({
        jobId,
        stepId: step.id,
        shown: { title: step.title, metadata: renderedMetadata },
        onScreen: () => onScreen,
        isGone: () => disposed,
        signal: unmounted.signal,
        // THE PASSKEY SIGNS ONLY WHAT WAS DRAWN (design f623e425 D3).
        // The only way this surface reaches it is a step that never
        // declared presence: the block is drawn now and the tap asked
        // for again, so the approver reads before signing.
        onUnseen: (unseen) => {
          revealed = true;
          renderAll();
          return `nothing was signed: your passkey would sign ${unseen.join(', ')}, which this page had not shown — it is shown now; read it and press again`;
        },
      });
    }

    // A begin refused 412 says the step no longer matches what this
    // surface shows — another writer moved it (backlog d82b5f60, review of
    // car 66de0e4b). What this mount rendered, "Decision saved" included,
    // is no longer what the step holds, and nothing was signed. So the
    // stale claim comes down, the host is asked to refresh, and the
    // approver is told to reopen the step rather than sign a copy this
    // mount can no longer vouch for.
    function stepMovedUnderUs() {
      progress = ['The step changed since it was shown, so nothing was signed — reopen it to read it as it stands'];
      if (typeof onUpdate === 'function') onUpdate();
    }

    async function sign(role) {
      const wasBusy = busy;
      busy = true;
      error = null;
      renderAll();
      try {
        let res = await fetch(`/api/jobs/${jobId}/steps/${step.id}/sign-offs`, {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ role }),
        });
        if (res.status === 422) {
          const refusal = await res
            .clone()
            .json()
            .catch(() => null);
          if (refusal && refusal.required === 'presence') {
            const ticket = await presenceTicket();
            res = await fetch(`/api/jobs/${jobId}/steps/${step.id}/sign-offs`, {
              method: 'POST',
              headers: {
                'Content-Type': 'application/json',
                'x-presence-ticket': ticket,
              },
              body: JSON.stringify({ role }),
            });
          }
        }
        if (!res.ok) {
          error = `Could not record the ${role} signature (${res.status}). ${await res.text()}`;
          return false;
        } else {
          // Re-read rather than assume: the server decides attribution
          // and the shape hash the stamp pins.
          const fresh = await fetch(`/api/jobs/${jobId}`).then((r) => (r.ok ? r.json() : null));
          const s = fresh && (fresh.steps || []).find((x) => x.id === step.id);
          if (s) stamps = Array.isArray(s.sign_offs) ? s.sign_offs : [];
          stale.delete(role);
          progress.push(`Signed as ${role}`);
          if (typeof onUpdate === 'function') onUpdate();
          return true;
        }
      } catch (e) {
        error = `Could not record the ${role} signature: ${e}`;
        if (e && e.status === 412) stepMovedUnderUs();
        return false;
      } finally {
        busy = wasBusy;
        renderAll();
      }
    }

    async function decide(d) {
      busy = true;
      error = null;
      progress = [];
      renderAll();
      try {
        // 1. The decision and the declared fields land in metadata
        //    FIRST — a stamp attests the step's shape, so the content
        //    being signed must already be in it. They travel through
        //    the step metadata PATCH, which merges ONLY the keys this
        //    surface owns against the row as it stands. The old idiom
        //    spread the page-load snapshot into a metadata PUT, which
        //    replaces wholesale — so any key another writer added
        //    after this page loaded was silently erased (the lost
        //    update that reverted a review's title/markdown on
        //    2026-09-02).
        const patch = {};
        declared.forEach((f) => {
          if (signedDoc.has(f.name)) return;
          if (nonEmptyString(fieldValues[f.name])) patch[f.name] = fieldValues[f.name];
        });
        patch.decision = d;
        if (commentTa.value.trim()) patch.comment = commentTa.value.trim();
        // A signature already on the step covers exactly what is in
        // it. When this gesture would change none of that, it is NOT
        // re-saved — a fresh decided_at alone would move the shape the
        // stamp pins and make the stamp stale (2026-09-05, 15:40:22).
        // The decided_at that stands is the one that was signed.
        const current = step.metadata || {};
        const unchanged = Object.keys(patch).every((k) => current[k] === patch[k]);
        if (stamps.length > 0 && unchanged) {
          progress.push(`Decision already recorded: ${d}`);
        } else {
          patch.decided_at = new Date().toISOString();
          const saved = await fetch(`/api/jobs/${jobId}/steps/${step.id}/metadata`, {
            method: 'PATCH',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify(patch),
          });
          if (!saved.ok) {
            error = `Could not record the decision (${saved.status}): ${await saved.text()}`;
            return;
          }
          // Fold the merged keys into the local cache the same way the
          // server just did; keys other writers own stay as loaded.
          step.metadata = Object.assign(step.metadata || {}, patch);
          progress.push(`Decision saved: ${d}`);
          // Every stamp attested the shape before this write; the
          // server has just marked them stale, and so does the roster.
          stamps.forEach((st) => st && stale.add(st.role));
        }
        if (d === 'changes-requested' && !changesRequestedCompletes) {
          if (typeof onUpdate === 'function') onUpdate();
          return;
        }
        // 2. The user's own signature, in the SAME gesture when exactly
        //    one role is outstanding — the single-signer case, which is
        //    most decisions. It runs AFTER the decision landed, because
        //    a stamp attests the step's shape (metadata included):
        //    on 2026-09-05 David signed first, this flow re-saved the
        //    decision with a fresh decided_at two seconds later, and the
        //    completion answered 409 stale — his own signature undone by
        //    the surface that asked for it. Nothing below writes
        //    metadata again. The server still enforces who may stamp;
        //    a refusal shows as the signature error and the decision
        //    stays recorded. (Per-role sign buttons remain for
        //    multi-party steps, where the ceremony is not this user's.)
        let left = outstanding();
        if (left.length === 1) {
          const signed = await sign(left[0]);
          if (!signed) return;
          left = outstanding();
        }
        // 3. Complete — unless other signatures are outstanding, in
        //    which case the decision is recorded and the roster says
        //    plainly what everyone is waiting on.
        if (left.length > 0) {
          error = null;
          progress.push(`Waiting on: ${left.join(', ')}`);
          renderAll();
          if (typeof onUpdate === 'function') onUpdate();
          return;
        }
        // The completion is sent BARE. A step that names sign-off roles
        // completes on the live passkey stamps it holds (design
        // 1ce67f7e): the signature above is the approval, and the server
        // asks no second proof of the request that flips the status.
        const complete = (ticket) => {
          const headers = { 'Content-Type': 'application/json' };
          if (ticket) headers['x-presence-ticket'] = ticket;
          return fetch(`/api/jobs/${jobId}/steps/${step.id}`, {
            method: 'PUT',
            headers,
            body: JSON.stringify({ status: 'completed' }),
          });
        };
        // A completion refused for PRESENCE ({required: "presence"}) is
        // the one shape that still takes a ticket on the completing
        // request: a presence step that names NO sign-off role, whose
        // completion is its only act. It is answered with ONE ceremony
        // on the step as shown, and ONE retry (3ce3c15f; P.completeOnce).
        // The ticket is the gateway's, minted by that ceremony for this
        // step and this person, and rides that retry alone. Never a
        // second ceremony.
        // (A step whose STAMPS do not carry it — a role unsigned, or a
        // signature past its age — is refused without that key, naming
        // the roles under missing_or_stale_roles: the roster below
        // offers those signatures again, and no ticket would help.)
        const refusedForPresence = P.refusedForPresence;
        const answer = await P.completeOnce({
          put: complete,
          ticket: presenceTicket,
          onAsk: () => {
            progress.push('Completing needs your passkey');
            renderAll();
          },
        });
        if (answer.failed) {
          const e = answer.failed;
          error = `Could not complete: ${e && e.message ? e.message : e}`;
          // The decision DID land: the host re-reads the step, and a
          // 412 also takes down the claim this mount can no longer
          // vouch for (d82b5f60).
          if (e && e.status === 412) stepMovedUnderUs();
          else if (typeof onUpdate === 'function') onUpdate();
          return;
        }
        const done = answer.done;
        const retried = answer.retried;
        if (!done.ok) {
          // 400: a required-at-done contract this surface did not
          // satisfy — name it, never swallow it (v1's ApprovalSurface
          // sibling swallowed these, which is how a click could
          // silently do nothing). 409: stale stamps; the server's own
          // text names which roles. Only a retry refused for PRESENCE
          // again is "refused again after a fresh passkey tap" — a 409
          // after the tap is labelled by its own reason (d82b5f60).
          const again = retried && (await refusedForPresence(done));
          const text = await done.text();
          error = again
            ? `The completion was refused again after a fresh passkey tap — ${done.status}: ${text}`
            : `${done.status}: ${text}`;
          // The 409 names the roles whose stamps the server will not
          // accept; the roster offers those signatures again rather
          // than showing them as signed.
          try {
            const body = JSON.parse(text);
            (Array.isArray(body.missing_or_stale_roles) ? body.missing_or_stale_roles : []).forEach(
              (r) => stale.add(r),
            );
          } catch (_) {
            // Not JSON: the text itself is the whole explanation.
          }
          return;
        }
        progress.push('Completed');
        if (typeof onUpdate === 'function') onUpdate();
      } catch (e) {
        error = `Could not record the decision: ${e}`;
      } finally {
        busy = false;
        renderAll();
      }
    }

    const root = h(
      'div',
      { className: 'step-signoff' },
      contextDiv,
      fieldsDiv,
      signedDiv,
      h('div', { className: 'step-signoff-head' }, 'Signatures'),
      rolesDiv,
      h('div', { className: 'step-field' }, commentTa),
      progressDiv,
      errorDiv,
      actionsDiv,
    );
    renderAll();
    container.appendChild(root);
    overflowChecks.forEach((check) => check());

    // The case for action, resolved the DecisionContext way: the
    // step's own context wins without a fetch; otherwise one job read
    // supplies the packet-level briefing or the filed message.
    const own = nonEmptyString((step.metadata || {}).context_md);
    if (own) {
      renderContext(own, 'written for this step', true);
    } else {
      fetch(`/api/jobs/${jobId}`)
        .then((r) => (r.ok ? r.json() : null))
        .then((job) => {
          const jm = (job && job.metadata) || {};
          const ctx = nonEmptyString(jm.context_md);
          if (ctx) return renderContext(ctx, 'the packet’s briefing', false);
          const msg = nonEmptyString(jm.message);
          if (msg) return renderContext(msg, 'the packet as filed', false);
        })
        .catch(() => {
          // No context is a quiet absence, never a broken surface.
        });
    }

    return () => {
      disposed = true;
      unmounted.abort();
      overflowObservers.forEach((o) => o.disconnect());
      overflowObservers = [];
      root.remove();
    };
  }

  // The copies of presence.ts's functions, where signOffPlugin.test.ts
  // can hold them equal to the app's on generated inputs (CLAUDE.md §9a;
  // 6093cf13 — the pin used to reach signedText alone, and canonical and
  // notShown only through one empty-screen case). Pure functions of their
  // arguments: exposing them grants nothing.

  // The bundle registers its mount only once passkey-ceremony.js has
  // run. It is a classic script, so it cannot import; it adds the shared
  // file's script tag itself, from the directory it was served from, and
  // the host — which already waits for a registration — knows nothing of
  // it. A shared file that does not load registers a mount that says so,
  // rather than leaving the step blank: an approval surface that cannot
  // run its ceremony must not look like one that can.
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
    console.error('[sign-off-plugin] __boss_register_step_plugin not on window');
    return;
  }
  withPasskey(
    (shared) => {
      P = shared;
      ({ signedText, scrollNote, canonical, notShown } = shared);
      // The copies of presence.ts's functions, where signOffPlugin.test.ts
      // can hold them equal to the app's on generated inputs (CLAUDE.md
      // §9a; 6093cf13 — the pin used to reach signedText alone, and
      // canonical and notShown only through one empty-screen case). Pure
      // functions of their arguments: exposing them grants nothing.
      mount.signed = Object.freeze({ signedText, canonical, notShown, scrollNote });
      window.__boss_register_step_plugin('sign-off', mount);
    },
    (why) => {
      window.__boss_register_step_plugin('sign-off', (container) => {
        const p = document.createElement('p');
        p.className = 'step-error';
        p.textContent = `This sign-off surface cannot run: /plugins/passkey-ceremony.js — ${why}. Nothing can be signed here; reload the page.`;
        container.append(p);
      });
    },
  );
})();
