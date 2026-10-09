// The passkey ceremony, plugin half — ONE copy, shared by every step
// plugin that asks a passkey of its reader.
//
// WHY THIS FILE EXISTS (item 570c66e9, 2026-10-07). The ceremony lived
// inside sign-off.js alone, because "plugins are self-contained bundles"
// and a bundle cannot import the app's presence.ts. On 2026-10-07 a
// `human_only` step began to complete only on a passkey, and the incident
// `review` step — human-only, rendered by incident-review.js, whose
// completion was one bare PUT — would have had no surface that could
// finish it. The second plugin needed the ceremony; a second 150-line
// paste of the most-reviewed code in this directory would have been two
// copies of a security check under a comment asking they be kept alike
// (CLAUDE.md §9a). So it is here once, and both bundles use it.
//
// HOW A BUNDLE GETS IT. This directory is served whole at /plugins/*
// (the converge runner builds the step-plugins ConfigMap from every
// *.js here), and a plugin is a classic script the host adds with a
// <script> tag. A classic script cannot `import`, but it can add a
// script tag of its own: a bundle that needs this file adds
// /plugins/passkey-ceremony.js and registers its mount only once this
// has run (the `withPasskey` loader at the foot of sign-off.js and
// incident-review.js). The host already waits for a registration, so
// nothing in apps/web knows this file exists. It registers NO step kind
// and no row in step_plugins names it.
//
// WHAT IS HERE, each piece moved from sign-off.js with its reasoning:
//   - signedText, canonical, notShown, scrollNote — how what a passkey
//     signs is drawn, and the check that what would be signed IS drawn.
//     The app's copies are in apps/web/src/steps/presence.ts;
//     signOffPlugin.test.ts holds the two equal on generated inputs.
//   - ticket — the ceremony: refuse off screen, refuse what was not
//     drawn, begin, the passkey, finish, with the off-screen guard read
//     again after every await.
//   - refusedForPresence, completeOnce — the answer to a completion
//     refused {required: "presence"}: ONE tap, ONE retry, never a loop.
//
// Pure of any step kind: nothing here knows a decision, a role or a
// finding. Everything a surface owns — what it drew, whether it is still
// mounted, what to say when it had not drawn a key — is handed in.

(function () {
  if (window.__boss_passkey) return;

  // A signed value AS THE BYTES IT IS (backlog 6093cf13, adversarial
  // review of car 30674304): a string that reads as exactly itself is
  // drawn bare; any other — empty, spaced at an edge, multi-line, holding
  // a character that is not printable ASCII or an em dash, starting with
  // a quote, or reading as a number, boolean, null or JSON — is drawn in
  // double quotes with each such character written as \u{XXXX}. Anything
  // else is its indented JSON with the same escapes. So '42' and 42, a
  // bidi override, a zero-width space and a Cyrillic look-alike are all
  // visibly what they are. Key names and the title are drawn through it
  // too. The app's copy is signedText in apps/web/src/steps/presence.ts
  // (the reasoning is there); signOffPlugin.test.ts pins the two equal on
  // generated inputs, because a bundle cannot import it.
  const AS_ITSELF = /^[\x20-\x7E\u2014]$/u;
  const NOT_AS_ITSELF = /[^\x20-\x7E\n\u2014]/gu;
  const escaped = (c) =>
    `\\u{${(c.codePointAt(0) || 0).toString(16).toUpperCase().padStart(4, '0')}}`;
  const QUOTED = { '\\': '\\\\', '"': '\\"', '\n': '\\n\n', '\t': '\\t', '\r': '\\r' };
  const quoted = (s) =>
    `"${[...s]
      .map((c) =>
        Object.prototype.hasOwnProperty.call(QUOTED, c)
          ? QUOTED[c]
          : AS_ITSELF.test(c)
            ? c
            : escaped(c),
      )
      .join('')}"`;

  function readsAsItself(s) {
    if (s === '' || s.startsWith('"') || s.startsWith(' ') || s.endsWith(' ')) return false;
    if (![...s].every((c) => AS_ITSELF.test(c))) return false;
    try {
      JSON.parse(s);
      return false;
    } catch (_) {
      return true;
    }
  }

  function signedText(v) {
    if (typeof v === 'string') return readsAsItself(v) ? v : quoted(v);
    const s = JSON.stringify(v, null, 2);
    return s === undefined ? String(v) : s.replace(NOT_AS_ITSELF, escaped);
  }

  // What a value whose box scrolls says under it (6093cf13): rendered is
  // not read. The app's copy is scrollNote in presence.ts, pinned equal.
  function scrollNote(text) {
    const lines = text.split('\n').length;
    return `scrolls in its box: ${lines} ${lines === 1 ? 'line' : 'lines'}, ${[...text].length} characters. Read it to the end; your passkey signs all of it.`;
  }

  function canonical(v) {
    if (Array.isArray(v)) return `[${v.map(canonical).join(',')}]`;
    if (v !== null && typeof v === 'object') {
      return `{${Object.keys(v)
        .sort()
        .map((k) => `${JSON.stringify(k)}:${canonical(v[k])}`)
        .join(',')}}`;
    }
    const s = JSON.stringify(v);
    return s === undefined ? 'null' : s;
  }

  // What a passkey would sign in `shown` that `screen` (the step as this
  // surface last drew it, or null when it drew no signed content) does
  // not show as signed: 'title', then each metadata key missing or drawn
  // with another value. The app's copy is notShown in presence.ts, pinned
  // equal by signOffPlugin.test.ts.
  function notShown(shown, screen) {
    const keys = Object.keys(shown.metadata).sort();
    if (!screen) return ['title', ...keys];
    const out = shown.title === screen.title ? [] : ['title'];
    keys.forEach((k) => {
      if (
        !Object.prototype.hasOwnProperty.call(screen.metadata, k) ||
        canonical(screen.metadata[k]) !== canonical(shown.metadata[k])
      ) {
        out.push(k);
      }
    });
    return out;
  }

  // Presence ceremony (docs/design/presence.md): a presence-gated
  // step refuses a plain stamp or completion with 422
  // {required:"presence"}; the passkey then signs a challenge bound to
  // this step's CURRENT shape hash and the request retries with the
  // issued ticket. No fallback path (Q3).
  const b64uBytes = (s) => {
    const pad = s.length % 4 === 2 ? '==' : s.length % 4 === 3 ? '=' : '';
    return Uint8Array.from(atob(s.replace(/-/g, '+').replace(/_/g, '/') + pad), (c) =>
      c.charCodeAt(0),
    );
  };
  const bytesB64u = (buf) =>
    btoa(String.fromCharCode(...new Uint8Array(buf)))
      .replace(/\+/g, '-')
      .replace(/\//g, '_')
      .replace(/=+$/, '');

  // Unmounted mid-gesture (6093cf13): the step this gesture began on is
  // no longer on screen, so the passkey is never asked, and an answer it
  // already gave is never sent to be turned into a ticket.
  const offScreen = () =>
    new Error('Nothing was signed: this step is no longer on screen, so your passkey was not used for it.');

  // THE CEREMONY. `o` is what the surface owns:
  //   jobId, stepId
  //   shown     {title, metadata} — the step as the surface rendered it,
  //             its own write folded in; what the begin names.
  //   onScreen  () => {title, metadata} | null — what the signed block
  //             drew, read at the moment of the check.
  //   isGone    () => boolean — the surface was unmounted.
  //   signal    AbortSignal — aborted by the surface's cleanup, so a
  //             prompt still up comes down with it (7c53b1bf).
  //   onUnseen  (keys) => string — called when the passkey would sign a
  //             key the surface had not drawn; does what the surface does
  //             about it and returns the refusal's words.
  //
  // THE BEGIN NAMES WHAT THE SURFACE SHOWED (backlog fd7090cc, the
  // security re-review of 2026-09-25): the step as rendered — never a
  // fresh read. The gateway hashes it and refuses (412) a begin whose
  // shown step is not the step as it stands, so a plan swapped between
  // the render and the key press is never what the passkey signs. This
  // is the ONE place a surface's snapshot rightly leaves the page, and
  // it is not a write: assert/begin stores nothing on the step, it only
  // compares.
  async function ticket(o) {
    if (o.isGone()) throw offScreen();
    // THE PASSKEY SIGNS ONLY WHAT WAS DRAWN (design f623e425 D3): a key
    // of what the begin would name that the signed block did not draw
    // as it would be signed refuses here, before any request.
    const unseen = notShown(o.shown, o.onScreen());
    if (unseen.length > 0) throw new Error(o.onUnseen(unseen));
    const begin = await fetch('/api/auth/passkey/assert/begin', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        job_id: o.jobId,
        step_id: o.stepId,
        shown: { title: o.shown.title, metadata: o.shown.metadata },
      }),
    });
    if (begin.status === 409) throw new Error('No passkey enrolled — add one first.');
    if (!begin.ok) {
      // The gateway's refusal text names which of its steps refused
      // (job fetch, stored passkeys, challenge mint); the status alone
      // does not. The app's own copy of the ceremony says the same
      // since 2e893e27 (backlog f3436d99).
      const text = await begin.text().catch(() => '');
      const refused = new Error(`presence ceremony unavailable (${begin.status}): ${text}`);
      // The status travels with the error: a 412 means the step moved
      // under this surface, which its callers answer differently.
      refused.status = begin.status;
      throw refused;
    }
    const opts = await begin.json();
    if (o.isGone()) throw offScreen();
    let cred;
    try {
      cred = await navigator.credentials.get({
        signal: o.signal,
        publicKey: {
          challenge: b64uBytes(opts.publicKey.challenge).buffer,
          rpId: opts.publicKey.rpId || undefined,
          allowCredentials: (opts.publicKey.allowCredentials || []).map((c) => ({
            type: c.type,
            id: b64uBytes(c.id).buffer,
          })),
          userVerification: opts.publicKey.userVerification,
          timeout: opts.publicKey.timeout,
        },
      });
    } catch (err) {
      // A prompt the cleanup aborted is the refusal it is, not the
      // browser's AbortError.
      if (o.isGone()) throw offScreen();
      throw err;
    }
    if (!cred) throw new Error('Passkey prompt returned no credential.');
    if (o.isGone()) throw offScreen();
    const a = cred.response;
    const finish = await fetch('/api/auth/passkey/assert/finish', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        challenge_id: opts.challenge_id,
        credential: {
          id: cred.id,
          rawId: bytesB64u(cred.rawId),
          type: cred.type,
          response: {
            authenticatorData: bytesB64u(a.authenticatorData),
            clientDataJSON: bytesB64u(a.clientDataJSON),
            signature: bytesB64u(a.signature),
            userHandle: a.userHandle ? bytesB64u(a.userHandle) : null,
          },
        },
      }),
    });
    if (!finish.ok) {
      // e.g. 410 'challenge already spent or expired — begin again',
      // or the verifier's own reason on a 401.
      const text = await finish.text().catch(() => '');
      throw new Error(`assertion rejected (${finish.status}): ${text}`);
    }
    const { ticket: issued } = await finish.json();
    // Unmounted while the finish was in flight: the ticket is never
    // used, so nothing lands for a step no longer on screen; unspent,
    // it lapses in its two minutes (7c53b1bf).
    if (o.isGone()) throw offScreen();
    return issued;
  }

  // Whether a response is the refusal a passkey answers: 422 whose body
  // says required: "presence". A 422 is also a malformed body, so the
  // status alone cannot say which it was.
  async function refusedForPresence(res) {
    if (res.status !== 422) return false;
    const refusal = await res
      .clone()
      .json()
      .catch(() => null);
    return Boolean(refusal && refusal.required === 'presence');
  }

  // A completion refused for PRESENCE is answered with ONE ceremony on
  // the step as shown, and ONE retry carrying the ticket that ceremony
  // was issued (3ce3c15f). Never a second ceremony.
  //   put(ticket?)  sends the completion; called bare first.
  //   ticket()      runs the ceremony.
  //   onAsk()       called before the ceremony, so the surface can say a
  //                 passkey is being asked for.
  // Answers {done, retried} — or {done, retried: false, failed} when the
  // CEREMONY failed, `failed` being its error (the completion is not
  // re-sent). An error thrown by `put` itself is the caller's: a request
  // that threw has no answer.
  async function completeOnce(o) {
    let done = await o.put();
    if (!(await refusedForPresence(done))) return { done, retried: false };
    if (o.onAsk) o.onAsk();
    let issued;
    try {
      issued = await o.ticket();
    } catch (e) {
      return { done, retried: false, failed: e };
    }
    done = await o.put(issued);
    return { done, retried: true };
  }

  window.__boss_passkey = Object.freeze({
    signedText,
    canonical,
    notShown,
    scrollNote,
    ticket,
    refusedForPresence,
    completeOnce,
  });
})();
