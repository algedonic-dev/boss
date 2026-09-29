<script lang="ts">
  // /it/registry/credentials — the credentials registry drawn, and its
  // lifecycle offered as packets (backlog 851259b9; design 76155676
  // step 3, David 2026-09-27). What each cell means, where provenance
  // comes from, and why the page can hold no value is credentials.ts's
  // header; this file only draws it.
  //
  // READING ORDER: the registry read decides the page — failed says so
  // and draws nothing, because an unreachable registry is not an empty
  // one. Then one card per row: who mints it and from what, what it can
  // do, where it lives and who reads it, when it last changed and when
  // it is next due, its gaps named, and the two packets a row can file.
  // Declaring a new row sits above the cards. The broker-rule read only
  // colours provenance; failing, it is said on each card it affects.
  import { onMount } from 'svelte';
  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import EntityLink from '@boss/web-kit/ui/EntityLink.svelte';
  import WriteGate from '@boss/web-kit/ui/WriteGate.svelte';
  import { session } from '@boss/web-kit/session/session.svelte';
  import type { Remote } from '../../data/remote';
  import {
    declareBody,
    declareIdProblem,
    fetchBrokerRules,
    fetchCredentials,
    fileJob,
    gapsOf,
    lastRotationText,
    nextDueText,
    provenanceOf,
    retireBody,
    rotateBody,
    scopesText,
    type BrokerRule,
    type Credential,
    type JobBody,
    type Settled,
  } from './credentials';

  let registry = $state<Remote<ReadonlyArray<Credential>>>({ kind: 'loading' });
  let rules = $state<Settled<ReadonlyArray<BrokerRule>> | null>(null);

  onMount(() => {
    void (async () => {
      const [c, r] = await Promise.all([fetchCredentials(), fetchBrokerRules()]);
      registry = c;
      rules = r;
    })();
  });

  const viewerId = $derived(session.value.kind === 'ready' ? session.value.user.id : null);

  // One filing per action key (`rotate:<id>`, `retire:<id>`, `declare`):
  // in flight, filed (with the packet), or refused (with the reason).
  type Filing = { kind: 'filing' } | { kind: 'filed'; id: string } | { kind: 'refused'; error: string };
  let filings = $state<Readonly<Record<string, Filing>>>({});

  async function file(key: string, body: JobBody): Promise<void> {
    if (filings[key]?.kind === 'filing') return;
    filings = { ...filings, [key]: { kind: 'filing' } };
    try {
      const id = await fileJob(body);
      filings = { ...filings, [key]: { kind: 'filed', id } };
    } catch (e) {
      filings = { ...filings, [key]: { kind: 'refused', error: e instanceof Error ? e.message : String(e) } };
    }
  }

  let declareId = $state('');
  const ids = $derived(registry.kind === 'ready' ? registry.data.map((c) => c.id) : []);
  const declareProblem = $derived(declareIdProblem(declareId.trim(), ids));

  const gapTotal = $derived(registry.kind === 'ready' ? registry.data.reduce((n, c) => n + gapsOf(c).length, 0) : 0);
  const provenances = $derived(
    registry.kind === 'ready' && rules !== null
      ? registry.data.map((c) => provenanceOf(c, rules as Settled<ReadonlyArray<BrokerRule>>).kind)
      : [],
  );
  const count = (k: string): number => provenances.filter((p) => p === k).length;
</script>

{#snippet filed(key: string, what: string)}
  {@const f = filings[key]}
  {#if f?.kind === 'filing'}
    <span class="cr-quiet">filing…</span>
  {:else if f?.kind === 'filed'}
    <span class="cr-filed">{what} filed: <EntityLink kind="job" id={f.id} label={f.id.slice(0, 8)} mono /></span>
  {:else if f?.kind === 'refused'}
    <span class="cr-fail load-failed">{what} not filed: {f.error}</span>
  {/if}
{/snippet}

<div class="cr-root">
  <PageHeader
    eyebrow="IT · Registry · what the estate holds access with"
    title="Credentials"
    subtitle="The credentials registry: who mints each credential, what it can do, where its value lives and who reads it, and when it last changed. Locations only — this page holds no value and reads none. Declaring, rotating and retiring each file a packet."
  />

  {#if registry.kind === 'loading'}
    <p class="cr-quiet">Reading the credentials registry…</p>
  {:else if registry.kind === 'failed'}
    <p class="cr-fail load-failed">
      The credentials registry did not answer: {registry.error}. A failed read is not an empty registry, so
      nothing is drawn.
    </p>
  {:else}
    <p class="cr-summary">
      {registry.data.length} credential{registry.data.length === 1 ? '' : 's'}
      {#if rules !== null && rules.kind === 'ready'}
        · {count('root')} root · {count('broker')} minted by the broker · {count('hand')} placed by hand
      {/if}
      · <span class:cr-gap={gapTotal > 0}>{gapTotal} gap{gapTotal === 1 ? '' : 's'}</span>
    </p>

    <WriteGate>
      <form
        class="cr-declare"
        aria-label="Declare a credential"
        onsubmit={(e) => {
          e.preventDefault();
          if (declareProblem === null && viewerId !== null) void file('declare', declareBody(declareId.trim(), viewerId));
        }}
      >
        <label for="cr-declare-id">Declare a credential the registry does not hold</label>
        <div class="cr-declare-row">
          <input
            id="cr-declare-id"
            class="mono"
            type="text"
            autocomplete="off"
            spellcheck="false"
            placeholder="credential id, e.g. github-app-installation"
            bind:value={declareId}
          />
          <button type="submit" disabled={declareProblem !== null || viewerId === null || filings.declare?.kind === 'filing'}>
            File a declare item
          </button>
        </div>
        <small>
          {#if declareId.trim() !== '' && declareProblem !== null}
            <span class="cr-warn">{declareProblem}</span>
          {:else}
            The name only, never a value. Files a backlog item: a row is declared in the tenant's
            seeds/credentials.toml and published by boss tenant publish, which no page reaches.
          {/if}
        </small>
        {@render filed('declare', 'Declare item')}
      </form>
    </WriteGate>

    {#if registry.data.length === 0}
      <p class="cr-notice">The registry holds no credential. An instance declares its rows; until it does, there are none.</p>
    {/if}

    {#each registry.data as c (c.id)}
      {@const p = rules === null ? null : provenanceOf(c, rules)}
      {@const gaps = gapsOf(c)}
      {@const scopes = scopesText(c)}
      {@const last = lastRotationText(c)}
      {@const next = nextDueText(c)}
      <section class="cr-card" aria-label={c.id}>
        <h2>{c.id} <span class="mono cr-kind">{c.kind}</span></h2>
        <dl class="cr-facts">
          <dt>Provenance</dt>
          <dd class="cr-prov">
            {#if p === null}
              reading the broker's rules…
            {:else if p.kind === 'root'}
              <strong>Root — placed by David.</strong> Its value lives in the broker's root Secret; the broker mints
              with it and never rotates it.
            {:else if p.kind === 'broker'}
              <strong>Minted by the broker</strong> — <span class="mono">{p.handlers.join(', ')}</span>, by rule
              <span class="mono">{p.rules.join(', ')}</span>.
              <span class="cr-gap">Minted from: not recorded — the registry holds no link to the root it is minted with.</span>
            {:else if p.kind === 'hand'}
              <strong>Placed by hand</strong> — no broker rule rotates it.
            {:else}
              <span class="cr-fail-inline load-failed">could not tell — the broker's rules did not answer: {p.why}</span>
            {/if}
          </dd>
          <dt>Issuer</dt>
          <dd>{c.issuer}</dd>
          <dt>Principal</dt>
          <dd>{c.principal}</dd>
          <dt>Scopes</dt>
          <dd class="mono" class:cr-gap={scopes.gap}>{scopes.text}</dd>
          <dt>Stored at</dt>
          <dd class="mono">{c.storageLocation}</dd>
          <dt>Consumers</dt>
          <dd>
            {#if c.consumers.length === 0}
              <span class="cr-gap">none declared</span>
            {:else}
              <ul class="cr-consumers">
                {#each c.consumers as k, i (i)}
                  <li><span class="mono cr-ckind">{k.kind}</span> {k.location}</li>
                {/each}
              </ul>
            {/if}
          </dd>
          <dt>Last rotation</dt>
          <dd class="mono" class:cr-gap={last.gap}>{last.text}</dd>
          <dt>Next rotation due</dt>
          <dd class:cr-gap={next.gap}>{next.text}</dd>
          {#if c.notes !== ''}
            <dt>Notes</dt>
            <dd class="cr-notes">{c.notes}</dd>
          {/if}
        </dl>

        {#if gaps.length > 0}
          <p class="cr-gaps"><span class="cr-gap">Gaps:</span> {gaps.join(' · ')}</p>
        {/if}

        <WriteGate>
          <div class="cr-actions">
            <button
              type="button"
              disabled={viewerId === null || filings[`rotate:${c.id}`]?.kind === 'filing' || filings[`rotate:${c.id}`]?.kind === 'filed'}
              onclick={() => viewerId !== null && file(`rotate:${c.id}`, rotateBody(c.id, viewerId))}
            >
              Rotate
            </button>
            <button
              type="button"
              disabled={viewerId === null || filings[`retire:${c.id}`]?.kind === 'filing' || filings[`retire:${c.id}`]?.kind === 'filed'}
              onclick={() => viewerId !== null && file(`retire:${c.id}`, retireBody(c, viewerId))}
            >
              Retire
            </button>
            <small class="cr-quiet">
              Rotate files rotate-a-credential on {c.id}; you scope it{p?.kind === 'broker'
                ? ', and the broker mints, installs, verifies and revokes'
                : ', and every later phase is done by hand on the packet'}. Retire files a backlog item: no protocol
              retires a credential yet.
            </small>
          </div>
          <div class="cr-results">
            {@render filed(`rotate:${c.id}`, 'Rotation')}
            {@render filed(`retire:${c.id}`, 'Retire item')}
          </div>
        </WriteGate>
      </section>
    {/each}
  {/if}
</div>

<style>
  .cr-root { padding: 0 32px 32px; }
  .cr-quiet { color: var(--static); font-size: 13px; }
  .cr-summary { color: var(--static); font-size: 13px; margin: 4px 0 12px; }
  .cr-fail {
    color: var(--warn); border: 1px solid var(--warn);
    padding: 8px 12px; font-size: 13px;
  }
  .cr-fail-inline { color: var(--warn); }
  .cr-warn { color: var(--warn); }
  .cr-notice {
    color: var(--fog); border: 1px solid var(--hairline);
    padding: 8px 12px; font-size: 13px; max-width: 90ch;
  }
  /* A gap is drawn as missing: warn-coloured and dashed-underlined, so
     it cannot be read as a value. */
  .cr-gap { color: var(--warn); text-decoration: underline dashed; text-underline-offset: 3px; }
  .mono { font-family: var(--font-mono); font-variant-numeric: tabular-nums; }
  .cr-declare { border: 1px solid var(--hairline); padding: 12px 16px; margin: 0 0 8px; font-size: 13px; }
  .cr-declare label { display: block; font-weight: 500; margin-bottom: 6px; }
  .cr-declare-row { display: flex; gap: 8px; flex-wrap: wrap; margin-bottom: 4px; }
  .cr-declare input { flex: 1 1 24ch; min-width: 0; padding: 4px 8px; }
  .cr-declare small { display: block; color: var(--static); margin-bottom: 4px; }
  .cr-card { border: 1px solid var(--hairline); padding: 12px 16px; margin-top: 16px; }
  .cr-card h2 { font-size: 16px; font-weight: 600; margin: 0 0 8px; overflow-wrap: anywhere; }
  .cr-kind { font-size: 12px; font-weight: 400; color: var(--static); margin-left: 6px; }
  .cr-facts {
    display: grid; grid-template-columns: max-content 1fr;
    gap: 4px 16px; margin: 0; font-size: 13px;
  }
  .cr-facts dt { color: var(--static); }
  .cr-facts dd { margin: 0; overflow-wrap: anywhere; min-width: 0; }
  .cr-prov .cr-gap { display: block; margin-top: 2px; }
  .cr-consumers { margin: 0; padding-left: 16px; }
  .cr-ckind { color: var(--static); font-size: 12px; }
  .cr-notes { color: var(--text-faint); }
  .cr-gaps { font-size: 13px; margin: 10px 0 0; }
  .cr-actions { display: flex; gap: 8px; align-items: center; flex-wrap: wrap; margin-top: 12px; }
  .cr-actions small { flex: 1 1 40ch; }
  .cr-results { display: flex; flex-direction: column; gap: 2px; font-size: 13px; margin-top: 4px; }
  .cr-filed { color: var(--static); }
  @media (max-width: 640px) {
    .cr-root { padding: 0 16px 24px; }
    .cr-facts { grid-template-columns: 1fr; }
    .cr-facts dt { margin-top: 6px; }
  }
</style>
