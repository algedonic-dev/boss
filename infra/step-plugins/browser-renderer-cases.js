// Versioned fixed synthetic cases. Both the published observer and owning
// browser tests execute this definition; no live API is available to it.
(function () {
  'use strict';
  const JOB = 'synthetic-question';
  const DESIGN = '5da17ace-7e35-4eae-a7c6-20e063c1791a';
  const jobPath = '/api/jobs/' + JOB;
  const question = (metadata = {}, status = 'ready') => ({
    id: 'synthetic-step', kind: 'answer-question', title: 'Answer the question', status,
    fields: [{ name: 'verdict', field_type: 'answered', required: true }, { name: 'answer', field_type: 'string', required: true }],
    metadata: { question: 'Which maintenance window should we use?', context_md: 'The customer needs an evening window.', ...metadata },
  });
  const ordinary = { title: 'Service visit', metadata: { parent_job_id: 'synthetic-source' }, steps: [] };
  const linked = { title: 'Old alarm report', metadata: {}, steps: [{ id: 'draft', status: 'completed', metadata: { design_id: DESIGN } }] };
  const definitions = [
    { id: 'question', step: question(), replies: { [jobPath]: [{ body: ordinary }] } },
    { id: 'decision', step: question({ context_md: '' }), replies: { [jobPath]: [{ body: linked }], ['/api/jobs/' + DESIGN]: [{ body: { title: 'Alarm meanings', metadata: { no_open_questions: true, questions: [], markdown: 'Red requires immediate action.' } } }] } },
    { id: 'contradiction', step: question(), replies: { [jobPath]: [{ body: linked }], ['/api/jobs/' + DESIGN]: [{ body: { title: 'Transport choice', metadata: { no_open_questions: true, questions: [{ anchor: 'transport', title: 'Enable roaming?', proposal: 'Keep SSH.' }] } } }] } },
    { id: 'retry', step: question(), replies: { [jobPath]: [{ status: 503, body: {} }, { body: ordinary }] } },
    { id: 'completed', step: question({ verdict: 'answered', answer: 'Tuesday evening' }, 'completed'), replies: { [jobPath]: [{ status: 503, body: {} }] } },
    { id: 'missing', step: question({ question: '' }), replies: { [jobPath]: [{ body: ordinary }] } },
  ];
  const pause = () => new Promise((resolve) => setTimeout(resolve, 10));
  async function run(mount) {
    if (typeof mount !== 'function') throw new Error('Renderer registration missing.');
    const results = [];
    const originalFetch = window.fetch;
    const host = document.getElementById('case-host');
    if (!host) throw new Error('Synthetic host missing.');
    try {
      for (const definition of definitions) {
        host.replaceChildren();
        const result = { id: definition.id, assertions: [], requests: [], error: null };
        const counters = {};
        let updates = 0;
        let cleanup;
        function assert(name, passed, actual) {
          result.assertions.push({ name, passed: Boolean(passed), actual: String(actual).slice(0, 8192) });
          if (!passed) throw new Error(name);
        }
        const button = (name) => [...host.querySelectorAll('button')].find((node) => node.textContent.trim() === name);
        async function text(value) {
          for (let tick = 0; tick < 100 && !host.textContent.includes(value); tick++) await pause();
          assert('visible: ' + value, host.textContent.includes(value), host.textContent);
        }
        window.fetch = async (url, init = {}) => {
          const path = String(url);
          const method = init.method || 'GET';
          const syntheticWrite = definition.id === 'question'
            && (method === 'PATCH' && path === jobPath + '/steps/synthetic-step/metadata'
              || method === 'PUT' && path === jobPath + '/steps/synthetic-step');
          const queue = method === 'GET' ? definition.replies[path] : null;
          const allowed = Boolean(queue || syntheticWrite);
          result.requests.push({ url: path, method, body: init.body || null, allowed });
          if (!allowed) throw new Error('Forbidden synthetic request.');
          if (syntheticWrite) return new Response('{}', { headers: { 'content-type': 'application/json' } });
          const index = counters[path] || 0;
          counters[path] = index + 1;
          const reply = queue[Math.min(index, queue.length - 1)];
          return new Response(JSON.stringify(reply.body), { status: reply.status || 200, headers: { 'content-type': 'application/json' } });
        };
        try {
          cleanup = mount(host, { step: definition.step, jobId: JOB, onUpdate: () => { updates++; } });
          if (definition.id === 'question') {
            await text('Which maintenance window should we use?');
            await text('The customer needs an evening window.');
            assert('related work link', host.querySelector('a[href="/ux/jobs/synthetic-source"]') !== null, host.textContent);
            assert('only declared verdict', !button('Approve'), host.textContent);
            button('Answer').click();
            const input = host.querySelector('textarea');
            assert('answer input', Boolean(input), host.textContent);
            input.value = 'Tuesday evening';
            input.dispatchEvent(new Event('input', { bubbles: true }));
            button('Record the answer').click();
            for (let tick = 0; tick < 100 && updates !== 1; tick++) await pause();
            assert('completion update', updates === 1, updates);
            const writes = result.requests.filter((request) => request.method !== 'GET');
            assert('metadata merge then status-only completion', JSON.stringify(writes.map(({ method, body }) => ({ method, body }))) === JSON.stringify([
              { method: 'PATCH', body: JSON.stringify({ verdict: 'answered', answer: 'Tuesday evening' }) },
              { method: 'PUT', body: JSON.stringify({ status: 'completed' }) },
            ]), JSON.stringify(writes));
          } else if (definition.id === 'decision') {
            await text('No answer needed'); await text('This decision is already recorded.');
            assert('no repeated decision write', !button('Record the answer') && !host.querySelector('textarea'), host.textContent);
          } else if (definition.id === 'contradiction') {
            await text('Review the linked design');
            assert('real question survives contradictory flag', !host.textContent.includes('No answer needed'), host.textContent);
          } else if (definition.id === 'retry') {
            await text('Could not load the question details.');
            assert('no answer before context read', !button('Record the answer'), host.textContent);
            button('Retry').click(); await text('Which maintenance window should we use?');
          } else if (definition.id === 'completed') {
            await text('Answered — answered. Tuesday evening');
            assert('completed answer read-only', !button('Record the answer'), host.textContent);
          } else {
            await text('No question has been recorded.');
            assert('no invented answer', !button('Record the answer'), host.textContent);
          }
          assert('all transport synthetic', result.requests.every((request) => request.allowed), JSON.stringify(result.requests));
        } catch (error) {
          result.error = String(error && error.message || error).slice(0, 1024);
          if (!result.assertions.length) result.assertions.push({ name: 'runner reached assertions', passed: false, actual: result.error });
        } finally {
          if (typeof cleanup === 'function') cleanup();
          host.replaceChildren();
        }
        results.push(result);
      }
    } finally { window.fetch = originalFetch; }
    return { schema: 1, cases: results };
  }
  window.__boss_renderer_cases = Object.freeze({ ids: definitions.map((definition) => definition.id), run });
})();
