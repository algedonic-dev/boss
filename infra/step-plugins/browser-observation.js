// Attended evidence only (design ec6fc29a). A client report is never boss prove.
(function () {
  'use strict';
  const LIMIT = 262144;
  const OUTPUT_LIMIT = 65536;
  const SANDBOX = 'allow-scripts';
  const CSP = "default-src 'none'; script-src data:; style-src 'unsafe-inline'; connect-src 'none'; form-action 'none'; base-uri 'none'; navigate-to 'none'";
  const hashPattern = /^[a-f0-9]{64}$/;
  async function digest(bytes) {
    if (!crypto.subtle) throw new Error('A secure browser with WebCrypto is required.');
    return Array.from(new Uint8Array(await crypto.subtle.digest('SHA-256', bytes)))
      .map((v) => v.toString(16).padStart(2, '0')).join('');
  }
  async function bounded(response, limit = LIMIT, created = false) {
    if ((response.status !== 200 && !(created && response.status === 201)) || response.redirected || !response.body) throw new Error('The exact asset could not be read.');
    const reader = response.body.getReader();
    const chunks = [];
    let size = 0;
    try {
      for (;;) {
        const { done, value } = await reader.read();
        if (done) break;
        size += value.byteLength;
        if (size > limit) throw new Error('The response exceeds its byte bound.');
        chunks.push(value);
      }
    } finally { await reader.cancel(); }
    const bytes = new Uint8Array(size);
    let offset = 0;
    for (const chunk of chunks) { bytes.set(chunk, offset); offset += chunk.length; }
    return bytes;
  }
  async function asset(binding, response) {
    if (!binding || !/^\/plugins\/[a-z0-9-]+\.(js|json)$/.test(binding.path)
        || !hashPattern.test(binding.sha256) || !Number.isSafeInteger(binding.bytes)
        || binding.bytes < 1 || binding.bytes > LIMIT) throw new Error('Invalid published asset binding.');
    const expected = new URL(binding.path, location.origin);
    if (response.url !== expected.href) throw new Error('The asset URL does not match its binding.');
    const bytes = await bounded(response);
    const observed = await digest(bytes);
    if (bytes.length !== binding.bytes || observed !== binding.sha256) throw new Error('The served bytes do not match the immutable source.');
    // Hash before decoding. A malformed UTF-8 script is never repaired silently.
    const text = new TextDecoder('utf-8', { fatal: true }).decode(bytes);
    return { bytes, text, observed, url: response.url, status: response.status,
      headers: Object.fromEntries(['etag', 'last-modified', 'cache-control', 'content-type'].map((key) => [key, response.headers.get(key)])) };
  }
  function dataScript(bytes) {
    let binary = '';
    for (const value of bytes) binary += String.fromCharCode(value);
    return '<script src="data:text/javascript;base64,' + btoa(binary) + '"></script>';
  }
  function output(value, ids) {
    if (!value || value.schema !== 1 || !Array.isArray(value.cases)
        || new TextEncoder().encode(JSON.stringify(value)).length > OUTPUT_LIMIT || value.cases.length !== ids.length) throw new Error('Incomplete or oversized case evidence.');
    const seen = new Set();
    for (const row of value.cases) {
      if (!row || !ids.includes(row.id) || seen.has(row.id) || !Array.isArray(row.assertions)
          || row.assertions.length < 1 || row.assertions.length > 32 || !Array.isArray(row.requests)
          || row.requests.length > 32 || !row.assertions.every((a) => a && typeof a.name === 'string'
            && a.name.length <= 256 && typeof a.passed === 'boolean' && typeof a.actual === 'string'
            && a.actual.length <= 8192) || !(row.error === null || typeof row.error === 'string')) throw new Error('Malformed case evidence.');
      for (const request of row.requests) {
        if (!request || typeof request.url !== 'string' || request.url.length > 2048
            || typeof request.method !== 'string' || !/^[A-Z]{1,16}$/.test(request.method)
            || !(request.body === null || typeof request.body === 'string' && request.body.length <= 8192)
            || typeof request.allowed !== 'boolean') throw new Error('Malformed synthetic transport record.');
      }
      seen.add(row.id);
    }
    return value;
  }
  function run(container, renderer, suite, ids, timeout = 15000) {
    const nonce = crypto.randomUUID();
    const frame = document.createElement('iframe');
    frame.setAttribute('sandbox', SANDBOX);
    frame.setAttribute('referrerpolicy', 'no-referrer');
    frame.title = 'Synthetic renderer observations';
    const encoder = new TextEncoder();
    const bootstrap = 'window.__boss_register_step_plugin = (kind, mount) => { if (kind === "answer-question") window.__observed_mount = mount; };';
    const invoke = `Promise.resolve().then(() => window.__boss_renderer_cases.run(window.__observed_mount)).then(result => parent.postMessage({nonce:${JSON.stringify(nonce)},result}, '*')).catch(() => parent.postMessage({nonce:${JSON.stringify(nonce)},error:'The synthetic runner failed.'}, '*'));`;
    const innerDocument = '<!doctype html><html><head><meta charset="utf-8"><meta http-equiv="Content-Security-Policy" content="' + CSP + '"></head><body><div id="case-host"></div>'
      + dataScript(encoder.encode(bootstrap)) + dataScript(suite.bytes) + dataScript(renderer.bytes) + dataScript(encoder.encode(invoke)) + '</body></html>';
    // Only the inner frame executes the pinned public bytes. The trusted
    // wrapper's frame-src fences the child's own document navigations;
    // navigate-to alone is not enforced by Chromium for opaque srcdoc.
    const relay = `(() => {
      function start() {
      const child = document.createElement('iframe');
      child.setAttribute('sandbox', 'allow-scripts');
      child.setAttribute('referrerpolicy', 'no-referrer');
      child.title = 'Isolated synthetic cases';
      window.addEventListener('message', event => {
        if (event.source !== child.contentWindow || event.origin !== 'null' || !event.data || event.data.nonce !== ${JSON.stringify(nonce)}) return;
        let encoded;
        try { encoded = JSON.stringify(event.data); } catch { return; }
        if (encoded.length > ${OUTPUT_LIMIT + 4096}) return;
        parent.postMessage(event.data, '*');
      });
      child.srcdoc = ${JSON.stringify(innerDocument)};
      document.body.appendChild(child);
      }
      const canary = document.createElement('iframe');
      canary.setAttribute('sandbox', 'allow-scripts');
      canary.setAttribute('referrerpolicy', 'no-referrer');
      canary.hidden = true;
      let supported = false;
      const timer = setTimeout(() => {
        document.removeEventListener('securitypolicyviolation', check);
        canary.remove();
        if (!supported) parent.postMessage({nonce:${JSON.stringify(nonce)},error:'Navigation isolation is unsupported.'}, '*');
      }, 500);
      function check(event) {
        if (supported || event.effectiveDirective !== 'frame-src') return;
        supported = true;
        clearTimeout(timer);
        document.removeEventListener('securitypolicyviolation', check);
        canary.remove();
        start();
      }
      document.addEventListener('securitypolicyviolation', check);
      canary.src = 'data:text/html,blocked-navigation-canary';
      document.body.appendChild(canary);
    })();`;
    frame.srcdoc = '<!doctype html><html><head><meta charset="utf-8"><meta http-equiv="Content-Security-Policy" content="' + CSP + '; frame-src about:' + '"></head><body>'
      + dataScript(encoder.encode(relay)) + '</body></html>';
    return new Promise((resolve, reject) => {
      function finish(error, result) {
        clearTimeout(timer);
        window.removeEventListener('message', receive);
        frame.remove();
        if (error) reject(error); else resolve(result);
      }
      function receive(event) {
        if (event.source !== frame.contentWindow || event.origin !== 'null' || !event.data || event.data.nonce !== nonce) return;
        try {
          if (event.data.error) throw new Error(event.data.error === 'Navigation isolation is unsupported.' ? event.data.error : 'The synthetic runner failed.');
          finish(null, output(event.data.result, ids));
        } catch (error) { finish(error); }
      }
      const timer = setTimeout(() => finish(new Error('The observation timed out; no complete result was received.')), timeout);
      window.addEventListener('message', receive);
      container.appendChild(frame);
    });
  }
  window.__boss_browser_observation = Object.freeze({ asset, bounded, digest, output, run, SANDBOX, CSP });
  function node(tag, text) {
    const element = document.createElement(tag);
    if (text !== undefined) element.textContent = text;
    return element;
  }
  function canonical(value) {
    if (Array.isArray(value)) return value.map(canonical);
    if (value && typeof value === 'object') return Object.fromEntries(Object.keys(value).sort().map((key) => [key, canonical(value[key])]));
    return value;
  }
  async function json(path, options = {}) {
    const response = await fetch(path, { credentials: 'same-origin', redirect: 'error', cache: 'no-store', ...options });
    const bytes = await bounded(response, LIMIT, options.method === 'POST');
    return JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(bytes));
  }
  function receipt(car) {
    const gate = (car.steps || []).find((step) => step.spec_slug === 'gate');
    let value = car.metadata && car.metadata.regate_receipt || gate && gate.metadata && gate.metadata.receipt;
    if (typeof value === 'string') value = JSON.parse(value);
    return value;
  }
  async function attachment(jobId, name, bytes) {
    const expected = await digest(bytes);
    const form = new FormData();
    form.append('target_kind', 'job'); form.append('target_id', jobId);
    form.append('file', new Blob([bytes], { type: 'application/octet-stream' }), name);
    const record = await json('/api/files', { method: 'POST', body: form });
    if (!record || !/^[a-f0-9-]{36}$/.test(record.id) || record.target?.kind !== 'job'
        || record.target?.id !== jobId || record.sha256 !== expected || record.size_bytes !== bytes.length) throw new Error('The attachment write did not return its exact binding.');
    const back = await bounded(await fetch('/api/files/' + record.id, { credentials: 'same-origin', redirect: 'error', cache: 'no-store' }));
    if (back.length !== bytes.length || await digest(back) !== expected) throw new Error('The attachment did not read back byte-for-byte.');
    return { id: record.id, sha256: expected, bytes: bytes.length };
  }
  function mount(container, { jobId, step, onUpdate }) {
    const root = node('section'); root.className = 'step-surface';
    const status = node('p', 'Loading the published observation binding…');
    const actions = node('div');
    const results = node('pre'); results.style.whiteSpace = 'pre-wrap';
    const frames = node('div');
    root.append(node('h3', 'Observe the deployed question renderer'),
      node('p', 'Attended browser evidence. Independent qualification is still required.'), status, actions, results, frames);
    container.appendChild(root);
    let disposed = false;
    let busy = false;
    let contract;
    let observed;
    function fail(error) { if (!disposed) status.textContent = String(error && error.message || error); }
    function button(label, action) {
      const element = node('button', label); element.type = 'button';
      element.addEventListener('click', () => { if (!busy && !disposed) void action(); });
      return element;
    }
    async function start() {
      busy = true; actions.replaceChildren(); observed = null; results.textContent = '';
      let fetched;
      try {
        const [renderer, suite] = await Promise.all([contract.asset, contract.suite].map(async (binding) => asset(binding,
          await fetch(binding.path, { credentials: 'same-origin', redirect: 'error', cache: 'no-store' }))));
        fetched = { renderer, suite };
        const result = await run(frames, renderer, suite, contract.cases);
        observed = { renderer, suite, result, complete: true };
        if (disposed) return;
        results.textContent = JSON.stringify(result, null, 2);
        status.textContent = 'Review every assertion and synthetic request before submitting. A report does not qualify the car.';
        actions.replaceChildren(button('Submit complete evidence', submit), button('Discard and run again', start));
      } catch (error) {
        fail(error);
        if (!disposed && fetched) {
          const result = { schema: 1, cases: [], incomplete: { error: String(error && error.message || error).slice(0, 1024) } };
          observed = { ...fetched, result, complete: false };
          results.textContent = JSON.stringify(result, null, 2);
          actions.replaceChildren(button('Submit incomplete evidence', submit), button('Discard and run again', start));
        } else if (!disposed) actions.replaceChildren(button('Retry observation', start));
      } finally { busy = false; }
    }
    async function submit() {
      if (!observed) return;
      busy = true; actions.replaceChildren();
      try {
        const reportBytes = new TextEncoder().encode(JSON.stringify(observed.result));
        const rendererFile = await attachment(jobId, 'observed-renderer.js', observed.renderer.bytes);
        const suiteFile = await attachment(jobId, 'observed-suite.js', observed.suite.bytes);
        const reportFile = await attachment(jobId, 'browser-observation.json', reportBytes);
        const value = { schema: 1, assurance: 'attended-browser-observation', binding: contract,
          asset: { url: observed.renderer.url, status: observed.renderer.status, headers: observed.renderer.headers,
            expected_sha256: contract.asset.sha256, observed_sha256: observed.renderer.observed, file: rendererFile },
          suite: { url: observed.suite.url, status: observed.suite.status, headers: observed.suite.headers,
            expected_sha256: contract.suite.sha256, observed_sha256: observed.suite.observed, file: suiteFile },
          report: reportFile, output: observed.result };
        const path = '/api/jobs/' + encodeURIComponent(jobId) + '/steps/' + encodeURIComponent(step.id);
        const written = await json(path + '/metadata/records', { method: 'POST', headers: { 'content-type': 'application/json' },
          body: JSON.stringify({ key: 'observation', value, expected_absence: true }) });
        const original = await json(path + '/records/observation');
        if (!['recorded', 'replayed'].includes(written.result) || original.job_id !== jobId || original.step_id !== step.id
            || original.key !== 'observation' || !original.actor || !original.recorded_at || !original.event_id
            || JSON.stringify(canonical(original.value)) !== JSON.stringify(canonical(value))) throw new Error('The immutable observation did not read back whole.');
        if (!observed.complete) {
          status.textContent = 'Incomplete evidence retained. A new observation is required; qualification remains refused.';
          return;
        }
        // The observation is the signer's key (rework of car b16e373d): this
        // same signed-in session stamps every role the step requires, after
        // the record, so the stamp's shape covers the observation it completes.
        for (const role of Array.isArray(step.sign_offs_required) ? step.sign_offs_required : []) {
          const stamped = await fetch(path + '/sign-offs', { method: 'POST', credentials: 'same-origin', redirect: 'error',
            headers: { 'content-type': 'application/json' }, body: JSON.stringify({ role }) });
          if (!stamped.ok) throw new Error('Evidence is retained, but the ' + role + ' sign-off was refused (HTTP ' + stamped.status + ').');
        }
        const response = await fetch(path, { method: 'PUT', credentials: 'same-origin', redirect: 'error',
          headers: { 'content-type': 'application/json' }, body: JSON.stringify({ status: 'completed' }) });
        if (!response.ok) throw new Error('Evidence is retained, but observation completion was refused (HTTP ' + response.status + ').');
        status.textContent = 'Evidence recorded. Independent qualification remains required. No car proof was completed.';
        onUpdate();
      } catch (error) {
        fail(error);
        if (!disposed) status.append(' Retained attachments are not discarded. Start a new observation if an original already exists.');
      } finally { busy = false; }
    }
    async function load() {
      try {
        const [published, job] = await Promise.all([json('/plugins/browser-renderer-contract.json'), json('/api/jobs/' + encodeURIComponent(jobId))]);
        if (published.schema !== 1 || published.version !== 1 || published.origin !== location.origin || job.kind !== 'browser-renderer-observation'
            || job.metadata?.car !== published.car) throw new Error('This observation does not name the published renderer car.');
        const car = await json('/api/jobs/' + encodeURIComponent(published.car));
        const green = receipt(car);
        if (car.id !== published.car || car.kind !== 'ship-a-change' || !green || green.verdict !== 'green'
            || green.head !== published.source_head || !Array.isArray(published.cases) || published.cases.length !== 6) throw new Error('The car and immutable source binding disagree.');
        contract = published;
        if (disposed) return;
        if (step.status === 'completed' || step.metadata?.observation) {
          status.textContent = 'An observation is already retained. Create a new observation packet for another run.';
          return;
        }
        status.textContent = 'The signed-in browser will fetch exact public bytes and run synthetic cases without a live question write.';
        actions.appendChild(button('Run observations', start));
      } catch (error) { fail(error); }
    }
    void load();
    return () => { disposed = true; root.remove(); };
  }
  if (typeof window.__boss_register_step_plugin === 'function') window.__boss_register_step_plugin('browser-observation', mount);
})();
