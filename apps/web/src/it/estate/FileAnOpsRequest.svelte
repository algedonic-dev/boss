<script lang="ts">
  // "File an ops request" — the Estate page's 05 (backlog 7aef2405,
  // David 2026-11-07: "In general, I just want to be using the website UI
  // for my controls").
  //
  // The form a person files any allowlisted verb from: pick the verb, its
  // host and its params, and the jobs API files the request as you — the
  // packet `boss ops` files, through the same create door. Filing is not
  // approving: a verb that needs approval waits on its approve step for
  // its approvers' passkey, and the host's runner checks every rule again.
  // Every verdict is the server's, shown verbatim (fileAnOpsRequest.ts).
  import WriteGate from '@boss/web-kit/ui/WriteGate.svelte';
  import Link from '@boss/web-kit/ui/Link.svelte';
  import { entityHref } from '@boss/web-kit/ui/entity-href';
  import { onMount } from 'svelte';
  import type { Remote } from '../../data/remote';
  import { waitsOn } from '../../jobs/fileTheRequest';
  import { argsFor, fileAnOpsRequest, loadOpsVerbs, paramHint, type Filed, type OpsVerb } from './fileAnOpsRequest';

  let verbs = $state<Remote<ReadonlyArray<OpsVerb>>>({ kind: 'loading' });
  let verbName = $state('');
  let host = $state('');
  let values = $state<Readonly<Record<string, string>>>({});
  let filing = $state(false);
  let answer = $state<Filed | null>(null);

  onMount(() => {
    void loadOpsVerbs().then((r) => {
      verbs = r;
    });
  });

  const chosen = $derived(verbs.kind === 'ready' ? (verbs.data.find((v) => v.name === verbName) ?? null) : null);

  function choose(name: string): void {
    verbName = name;
    values = {};
    answer = null;
    const v = verbs.kind === 'ready' ? verbs.data.find((x) => x.name === name) : undefined;
    host = v && v.hosts.length === 1 ? v.hosts[0]! : '';
  }

  function set(name: string, value: string): void {
    values = { ...values, [name]: value };
    answer = null;
  }

  async function file(): Promise<void> {
    if (chosen === null || host === '' || filing) return;
    const built = argsFor(chosen, values);
    if (built.kind === 'refused') {
      answer = built;
      return;
    }
    filing = true;
    answer = await fileAnOpsRequest({ host, verb: chosen.name, args: built.args });
    filing = false;
    // A filed request clears its fields, so a second press files nothing
    // until the person fills the form again.
    if (answer.kind === 'filed') values = {};
  }
</script>

<div class="faor" data-testid="file-an-ops-request">
  <p class="faor-line faor-muted">
    Files a request to one allowlisted verb (infra/ops/verbs) as you — the packet
    <code class="faor-mono">boss ops &lt;host&gt; &lt;verb&gt; &lt;args&gt;</code> files, which stays the way in when
    this site is dark. The host's runner checks it again before anything runs. Filing is not approving.
  </p>
  {#if verbs.kind === 'loading'}
    <p class="faor-line faor-muted">Reading the verb list…</p>
  {:else if verbs.kind === 'failed'}
    <p class="faor-line faor-refused load-failed" role="alert">Couldn't read the verb list: {verbs.error}</p>
  {:else}
    <WriteGate>
      <div class="faor-form">
        <!-- Each label names its control alone; the hint sits beside it,
             so a field's accessible name is its param's name. -->
        <div class="faor-field">
          <label class="faor-label" for="faor-verb">verb</label>
          <select id="faor-verb" value={verbName} onchange={(e) => choose(e.currentTarget.value)}>
            <option value="">choose a verb…</option>
            {#each verbs.data as v (v.name)}
              <option value={v.name}>{v.name}</option>
            {/each}
          </select>
        </div>
        {#if chosen}
          <p class="faor-line">
            {#if chosen.mutating}<strong class="faor-mutating">MUTATING</strong>{/if}
            {chosen.about}
          </p>
          <div class="faor-field">
            <label class="faor-label" for="faor-host">host</label>
            <select id="faor-host" value={host} onchange={(e) => { host = e.currentTarget.value; answer = null; }}>
              {#if chosen.hosts.length !== 1}<option value="">choose a host…</option>{/if}
              {#each chosen.hosts as h (h)}
                <option value={h}>{h}</option>
              {/each}
            </select>
          </div>
          {#each chosen.params as p (p.name)}
            <div class="faor-field">
              <label class="faor-label" for={`faor-param-${p.name}`}>{p.name}</label>
              {#if p.one_of}
                <select id={`faor-param-${p.name}`} value={values[p.name] ?? ''} onchange={(e) => set(p.name, e.currentTarget.value)}>
                  <option value="">{p.default !== null ? `default (${p.default})` : p.optional ? '(none)' : 'choose…'}</option>
                  {#each p.one_of as o (o)}
                    <option value={o}>{o}</option>
                  {/each}
                </select>
              {:else}
                <input
                  id={`faor-param-${p.name}`}
                  type="text"
                  autocomplete="off"
                  spellcheck="false"
                  value={values[p.name] ?? ''}
                  placeholder={p.default ?? ''}
                  oninput={(e) => set(p.name, e.currentTarget.value)}
                />
              {/if}
              <span class="faor-hint">{paramHint(p)}</span>
            </div>
          {/each}
          <p class="faor-line">{waitsOn(chosen)}</p>
          {#if chosen.plan_for}
            <p class="faor-line faor-muted">
              This is the read-only plan of {chosen.plan_for}: once it renders clean, its page offers
              “File the request” for the write itself.
            </p>
          {/if}
          <div>
            <button type="button" class="btn btn-primary" disabled={filing || host === ''} onclick={() => void file()}>
              {filing ? 'Filing…' : 'File the request'}
            </button>
          </div>
        {/if}
      </div>
    </WriteGate>
  {/if}
  {#if answer?.kind === 'refused'}
    <p class="faor-line faor-refused" role="alert">{answer.why}</p>
  {:else if answer?.kind === 'filed'}
    <p class="faor-line">
      Filed <Link to={entityHref('job', answer.id)} className="faor-mono">{answer.id}</Link> — it waits on its own
      steps there.
    </p>
  {/if}
</div>

<style>
  .faor { display: grid; gap: 8px; max-width: 72ch; }
  .faor-form { display: grid; gap: 8px; }
  .faor-line { margin: 0; font-size: 13px; line-height: 1.45; }
  .faor-muted { color: var(--static); }
  .faor-refused { color: var(--err); white-space: pre-wrap; overflow-wrap: anywhere; }
  .faor-mutating { color: var(--warn); font-weight: 500; margin-right: 6px; }
  .faor-field { display: grid; grid-template-columns: 9rem minmax(0, 1fr); gap: 2px 12px; align-items: baseline; font-size: 13px; }
  .faor-label { font-family: var(--font-mono); font-size: 12px; color: var(--static); overflow-wrap: anywhere; }
  .faor-field input, .faor-field select { font-family: var(--font-mono); font-size: 12px; min-width: 0; }
  .faor-hint { grid-column: 2; font-family: var(--font-mono); font-size: 11px; color: var(--static); overflow-wrap: anywhere; }
  :global(.faor-mono), .faor-mono { font-family: var(--font-mono); font-size: 12px; overflow-wrap: anywhere; }
  @media (max-width: 560px) {
    .faor-field { grid-template-columns: minmax(0, 1fr); }
    .faor-hint { grid-column: 1; }
  }
</style>
