<script lang="ts">
  // /it/registry/rules/{name} — edit a dispatcher rule: a draft form seeded from the active/latest
  // version, a version-history table, and the draft → publish/retire
  // lifecycle actions. Models the step-plugin detail page (LoadState
  // discriminated union + action/actionError pattern + version-history
  // table). Writes flow through ./ruleAuthoring.
  //
  // {name}==='new' renders NewRuleGuide instead: there is no create mode
  // (backlog 7d9df2fe, design ff1c3615). A rule created here was a
  // product rule no file names, which the dispatcher's boot seed retires
  // at the next restart; a rule starts as a file or a tenant seed.

  import Breadcrumb from '@boss/web-kit/ui/Breadcrumb.svelte';
  import Link from '@boss/web-kit/ui/Link.svelte';
  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import Section from '@boss/web-kit/ui/Section.svelte';
  import StatusChip from '@boss/web-kit/ui/StatusChip.svelte';
  import NewRuleGuide from './NewRuleGuide.svelte';
  import {
    listActiveRules,
    listVersions,
    createDraft,
    validateRule,
    publishRule,
    retireRule,
    buildRuleSpec,
    draftRefusal,
    formFromVersion,
    liveSource,
    type ActiveRules,
    type RuleVersion,
    type RuleStatus,
    type RuleSpec,
    type RuleTrigger,
  } from './ruleAuthoring';
  import { describeTrigger } from './cascadeToGraph';
  import { ruleProvenance } from './ruleProvenance';
  import { fetchRuleFirings, ruleActivity, type RuleFirings } from './ruleFirings';
  import {
    authorship,
    emittedKinds,
    fetchDispatcherSchedule,
    nextDue,
    ruleWhy,
    type ScheduleRead,
  } from './ruleSummary';
  import type { Remote } from '../data/remote';
  import { href } from '../router';

  type Props = { ruleName: string };
  let { ruleName }: Props = $props();

  let isNew = $derived(ruleName === 'new');

  // `missing` (the read answered: no versions) and `failed` (the read
  // did not answer) used to be one `error` arm painting its message as
  // the header subtitle, so a dispatcher outage read like a rule that
  // does not exist, with no marker for the outage crawl (backlog
  // d7732e88).
  type LoadState =
    | { kind: 'loading' }
    | { kind: 'missing'; message: string }
    | { kind: 'failed'; message: string }
    | { kind: 'ready'; versions: ReadonlyArray<RuleVersion> };

  let loadState = $state<LoadState>({ kind: 'loading' });

  // --- Editable form model ---------------------------------------------
  // `do` args are edited as an ordered list of key/value rows (rather than
  // free-form JSON) so the shape stays typed end-to-end. An empty-key row
  // is dropped on build.
  type ArgRow = { key: string; value: string };
  type DoRow = { handler: string; args: ArgRow[] };

  // The trigger is an event topic OR a schedule, exactly one — the
  // server's RawRule. A schedule rule serves no `on_event` at all (26 of
  // 80 live rules, 2026-09-27), and the form used to have no way to hold
  // one: it seeded `undefined`, and a topic typed in turned the rule into
  // an event rule on publish (backlog b38360ed). Both sides' fields are
  // held so switching loses no typing; buildRuleSpec sends the chosen one.
  let formName = $state('');
  let trigger = $state<RuleTrigger>('event');
  let onEvent = $state('');
  let cadence = $state('');
  let anchorDate = $state('');
  let businessCalendar = $state('');
  let whenExpr = $state('');
  let delay = $state('');
  let doRows = $state<DoRow[]>([{ handler: '', args: [] }]);

  // --- Action / validation feedback ------------------------------------
  let action = $state<string | null>(null);
  let actionError = $state<string | null>(null);
  let validateState = $state<{ ok: boolean; error: string | null } | null>(null);

  // --- The read-only summary (backlog 0034d5ef) -------------------------
  // What the rules list knows about a rule, on the page where Retire is
  // decided: the list's own reads, each beside the form rather than in
  // front of it, each with its own failure line. The schedule is read
  // only for a schedule rule (null = not asked).
  let rulesRead = $state<Remote<ActiveRules>>({ kind: 'loading' });
  let firings = $state<Remote<RuleFirings>>({ kind: 'loading' });
  let scheduleRead = $state<Remote<ScheduleRead> | null>(null);

  // Which history rows are open (backlog 9550ac95): each expands to its
  // content from the versions read already in hand.
  let expanded = $state<ReadonlyArray<number>>([]);

  function seedFrom(v: RuleVersion): void {
    const form = formFromVersion(v);
    formName = form.name;
    trigger = form.trigger;
    onEvent = form.on_event;
    cadence = form.cadence;
    anchorDate = form.anchor_date;
    businessCalendar = form.business_calendar;
    whenExpr = form.when;
    delay = form.delay;
    doRows = form.do.map((d) => ({ handler: d.handler, args: d.args.map((a) => ({ ...a })) }));
    if (doRows.length === 0) doRows = [{ handler: '', args: [] }];
  }

  async function readActiveRules(): Promise<Remote<ActiveRules>> {
    try {
      return { kind: 'ready', data: await listActiveRules() };
    } catch (e) {
      return { kind: 'failed', error: e instanceof Error ? e.message : String(e) };
    }
  }

  /** The summary's reads, fired without waiting — the editor paints
   *  whether or not they answer. */
  function loadSummary(scheduled: boolean): void {
    void readActiveRules().then((r) => {
      rulesRead = r;
    });
    void fetchRuleFirings().then((r) => {
      firings = r;
    });
    if (!scheduled) {
      scheduleRead = null;
      return;
    }
    if (scheduleRead === null) scheduleRead = { kind: 'loading' };
    void fetchDispatcherSchedule().then((r) => {
      scheduleRead = r;
    });
  }

  async function load(): Promise<void> {
    validateState = null;
    actionError = null;
    if (isNew) return;
    try {
      const versions = await listVersions(ruleName);
      if (versions.length === 0) {
        loadState = { kind: 'missing', message: `No versions found for "${ruleName}".` };
        return;
      }
      // Seed from the active version, else the latest (versions are
      // oldest-first, so the last entry is newest).
      const seed = versions.find((v) => v.status === 'active') ?? versions[versions.length - 1]!;
      seedFrom(seed);
      loadState = { kind: 'ready', versions };
      loadSummary(seed.schedule != null);
    } catch (e) {
      loadState = { kind: 'failed', message: e instanceof Error ? e.message : String(e) };
    }
  }

  function toggleVersion(version: number): void {
    expanded = expanded.includes(version)
      ? expanded.filter((v) => v !== version)
      : [...expanded, version];
  }

  $effect(() => {
    void ruleName;
    void load();
  });

  // --- do-row / arg-row editing ----------------------------------------
  function addDoRow(): void {
    doRows = [...doRows, { handler: '', args: [] }];
  }
  function removeDoRow(i: number): void {
    doRows = doRows.filter((_, idx) => idx !== i);
    if (doRows.length === 0) doRows = [{ handler: '', args: [] }];
  }
  function addArg(i: number): void {
    doRows = doRows.map((row, idx) =>
      idx === i ? { ...row, args: [...row.args, { key: '', value: '' }] } : row,
    );
  }
  function removeArg(i: number, ai: number): void {
    doRows = doRows.map((row, idx) =>
      idx === i ? { ...row, args: row.args.filter((_, j) => j !== ai) } : row,
    );
  }

  /** Build the API spec from the editor's $state via the shared, tested
   *  normalizer — so Validate and Save send byte-identical specs. */
  function buildSpec(): RuleSpec {
    return buildRuleSpec({
      name: formName,
      trigger,
      on_event: onEvent,
      cadence,
      anchor_date: anchorDate,
      business_calendar: businessCalendar,
      when: whenExpr,
      delay,
      do: doRows,
    });
  }

  // active = the published, live version → ok; retired → muted;
  // draft → warn (an unpublished draft is attention, and must read
  // differently from both the live and the retired rows).
  function statusTone(status: RuleStatus): 'ok' | 'warn' | 'muted' {
    return status === 'active' ? 'ok' : status === 'retired' ? 'muted' : 'warn';
  }

  async function runValidate(): Promise<void> {
    action = 'validate';
    actionError = null;
    try {
      validateState = await validateRule(buildSpec());
    } catch (e) {
      actionError = e instanceof Error ? e.message : String(e);
    } finally {
      action = null;
    }
  }

  // Everything after `action = 'save'` sits inside the try: buildSpec()
  // used to run before it, so a throw there (a schedule rule's undefined
  // on_event) left `action` set and all four buttons disabled reading
  // "Saving…" until a reload (backlog b38360ed). The draft restates the
  // source of the rule's live row, which the draft door requires of a
  // tenant's rule (backlog 636b9868).
  async function runSaveDraft(): Promise<void> {
    action = 'save';
    actionError = null;
    validateState = null;
    try {
      const spec = buildSpec();
      const refusal = draftRefusal(spec);
      if (refusal !== null) {
        actionError = refusal;
        return;
      }
      const versions = loadState.kind === 'ready' ? loadState.versions : [];
      await createDraft(spec, liveSource(versions));
      await load();
    } catch (e) {
      actionError = e instanceof Error ? e.message : String(e);
    } finally {
      action = null;
    }
  }

  async function runPublish(): Promise<void> {
    action = 'publish';
    actionError = null;
    try {
      await publishRule(ruleName);
      await load();
    } catch (e) {
      actionError = e instanceof Error ? e.message : String(e);
    } finally {
      action = null;
    }
  }

  async function runRetire(): Promise<void> {
    action = 'retire';
    actionError = null;
    try {
      const msg =
        `Retire rule "${ruleName}"?\n\n` +
        `The active version flips to retired. In-flight events already ` +
        `matched are unaffected; the dispatcher reloads its rules within ` +
        `about 30 seconds, and then this rule stops firing.`;
      if (!window.confirm(msg)) {
        action = null;
        return;
      }
      await retireRule(ruleName);
      await load();
    } catch (e) {
      actionError = e instanceof Error ? e.message : String(e);
    } finally {
      action = null;
    }
  }
</script>

{#if isNew}
  <NewRuleGuide />
{:else if loadState.kind === 'loading'}
  <div class="catalog theme-exec">
    <p class="empty">Loading…</p>
  </div>
{:else if loadState.kind === 'missing'}
  <div class="catalog theme-exec">
    <Breadcrumb to={href('/it/registry/rules')}>← All dispatcher rules</Breadcrumb>
    <PageHeader eyebrow="Platform · Dispatcher rule" title={ruleName} subtitle={loadState.message} />
  </div>
{:else if loadState.kind === 'failed'}
  <div class="catalog theme-exec">
    <Breadcrumb to={href('/it/registry/rules')}>← All dispatcher rules</Breadcrumb>
    <PageHeader
      eyebrow="Platform · Dispatcher rule"
      title={ruleName}
      subtitle="Versions unknown — the registry read failed"
    />
    <!-- load-failed + role=alert: the shared marker the outage crawl
         asserts (tests/mocked/_routes.ts FAILURE_MARKER), in the rules
         list's own words for its failed read (backlog d7732e88). -->
    <p class="empty load-failed" role="alert" style="margin:0 24px">Failed to load: {loadState.message}</p>
  </div>
{:else}
  {@const versions = loadState.versions}
  {@const hasDraft = versions.some((v) => v.status === 'draft')}
  {@const hasActive = versions.some((v) => v.status === 'active')}
  {@const active = versions.find((v) => v.status === 'active')}
  {@const current = active ?? versions[versions.length - 1]!}
  {@const source = liveSource(versions)}
  {@const enforced =
    rulesRead.kind === 'ready' ? (rulesRead.data.rules.find((r) => r.name === ruleName) ?? null) : null}
  {@const registry = rulesRead.kind === 'ready' ? rulesRead.data.authoredRegistry : null}
  {@const act = ruleActivity({ name: ruleName }, firings)}
  <div class="catalog theme-exec">
    <Breadcrumb to={href('/it/registry/rules')}>← All dispatcher rules</Breadcrumb>
    <PageHeader
      eyebrow="Platform · Dispatcher rule"
      title={ruleName}
      subtitle={`${versions.length} version${versions.length === 1 ? '' : 's'}${active ? ` · active v${active.version}` : ' · no active version'}`}
    />

    <!-- What the rules list knows about this rule (backlog 0034d5ef),
         read-only, above the controls it informs: Retire decided without
         last-fired or failing is a guess. -->
    <section class="rule-summary" aria-label="Rule summary" style="padding:0 24px 12px">
      <dl style="display:grid; grid-template-columns:max-content 1fr; gap:4px 16px; margin:0; font-size:13px">
        <dt style="color:var(--static)">Why</dt>
        <dd class="why" style="margin:0; white-space:pre-line">
          {rulesRead.kind === 'ready' ? ruleWhy(enforced, registry) : rulesRead.kind === 'loading' ? '…' : 'unknown'}
        </dd>
        <dt style="color:var(--static)">Declared by</dt>
        <dd class="declared-by" style="margin:0">
          {#if rulesRead.kind === 'loading'}
            …
          {:else if rulesRead.kind === 'failed'}
            unknown
          {:else if enforced}
            {@const prov = ruleProvenance(enforced, registry)}
            <span style={prov.kind === 'drift' ? 'color:var(--err)' : undefined}>{prov.label} — {prov.why}</span>
          {:else}
            {source ?? 'product'} — no version is active, so nothing enforces it
          {/if}
        </dd>
        <dt style="color:var(--static)">Last fired</dt>
        <dd class="last-fired" style="margin:0" title={act.lastFiredWhy}>{act.lastFired}</dd>
        <dt style="color:var(--static)">Dead-letters</dt>
        <dd class="dead-letters" class:failing={act.failing} title={act.deadLettersWhy}
            style={act.failing ? 'margin:0; color:var(--err)' : 'margin:0'}>
          {#if act.deadLetterJob}
            <Link to={href(`/jobs/${act.deadLetterJob}`)}>{act.deadLetters}</Link>
          {:else}
            {act.deadLetters}
          {/if}
        </dd>
        {#if scheduleRead !== null}
          {@const due =
            scheduleRead.kind === 'ready'
              ? nextDue(ruleName, scheduleRead.data)
              : { text: scheduleRead.kind === 'loading' ? '…' : 'unknown', why: '' }}
          <dt style="color:var(--static)">Next due</dt>
          <dd class="next-due" style="margin:0" title={due.why}>{due.text}</dd>
        {/if}
        <dt style="color:var(--static)">Emits</dt>
        <dd class="emits" style="margin:0">
          {rulesRead.kind === 'ready'
            ? emittedKinds(current, rulesRead.data.handlerEmits)
            : rulesRead.kind === 'loading' ? '…' : 'unknown'}
        </dd>
      </dl>
      <!-- One marked line per read that failed (load-failed + role=alert,
           the outage crawl's marker): a failed read is never painted as a
           rule with nothing to say. -->
      {#if rulesRead.kind === 'failed'}
        <p class="empty load-failed" role="alert" style="margin:8px 0 0; text-align:left">
          Couldn’t read the rule registry: {rulesRead.error}
        </p>
      {/if}
      {#if firings.kind === 'failed'}
        <p class="empty load-failed" role="alert" style="margin:8px 0 0; text-align:left">
          Couldn’t read the firing record: {firings.error}
        </p>
      {/if}
      {#if scheduleRead?.kind === 'failed'}
        <p class="empty load-failed" role="alert" style="margin:8px 0 0; text-align:left">
          Couldn’t read the schedule: {scheduleRead.error}
        </p>
      {/if}
    </section>

    <!-- The reload half was stale: the dispatcher polls dispatcher_rules
         every 30s and rebuilds its runners (backlog 1e576baf). The file
         half is the drift the boot seed reports as `behind` (7d9df2fe).
         A tenant's rule is versioned here as instance data, and the
         export is what levels its seed file (backlog 636b9868, design
         e187198f: the instance is the truth). -->
    <p class="empty publish-banner" style="padding:0 24px 8px; color:var(--warn)">
      {#if source?.startsWith('tenant:')}
        A version published here is live within about 30 seconds. This rule is declared by
        {source}, so a version saved here is instance data: the tenant's seeds/rules.toml
        lags it until boss tenant export brings that file level.
      {:else}
        A version published here is live within about 30 seconds, and runs ahead of the
        file that authors this rule until that file's version is raised to match:
        infra/dispatcher/rules/{ruleName}.toml.
      {/if}
    </p>

    <!-- Lifecycle actions -->
    <div style="padding:0 24px 16px; display:flex; gap:12px; align-items:center; flex-wrap:wrap">
      <button
        type="button"
        class="btn"
        onclick={runValidate}
        disabled={action !== null}
        title="Dry-run the draft against the dispatcher parser"
      >
        {action === 'validate' ? 'Validating…' : 'Validate'}
      </button>
      <button
        type="button"
        class="btn btn-primary"
        onclick={runSaveDraft}
        disabled={action !== null}
        title="Persist a new draft version (validated server-side)"
      >
        {action === 'save' ? 'Saving…' : 'Save draft'}
      </button>
      <button
        type="button"
        class="btn"
        onclick={runPublish}
        disabled={!hasDraft || action !== null}
        title={hasDraft ? 'Activate the latest draft' : 'No draft to publish'}
      >
        {action === 'publish' ? 'Publishing…' : 'Publish draft'}
      </button>
      <button
        type="button"
        class="btn"
        onclick={runRetire}
        disabled={!hasActive || action !== null}
        title={hasActive ? 'Retire the active version' : 'No active version to retire'}
      >
        {action === 'retire' ? 'Retiring…' : 'Retire'}
      </button>
      {#if validateState}
        {#if validateState.ok}
          <span style="color:var(--ok); font-size:13px">✓ Valid</span>
        {:else}
          <span style="color:var(--err); font-size:13px">✗ {validateState.error}</span>
        {/if}
      {/if}
      {#if actionError}
        <span class="action-error" role="alert" style="color:var(--err); font-size:13px">{actionError}</span>
      {/if}
    </div>

    <div class="tab-grid">
      <!-- Editor form -->
      <Section title="Rule">
        <div style="display:grid; gap:12px; max-width:800px">
          <div>
            <div style="font-size:12px; color:var(--static); margin-bottom:2px">
              Name
              <span style="color:var(--static)"> — the rule's permanent identity (not editable)</span>
            </div>
            <!-- No placeholder: the field is read-only and always seeded,
                 and the one it had named a rule with no live version
                 (backlog 3a42c67b). -->
            <input
              bind:value={formName}
              readonly
              aria-label="Name"
              class="mono"
              style="padding:6px; font-size:13px; width:100%"
            />
          </div>
          <fieldset class="trigger" style="border:0; padding:0; margin:0">
            <legend style="font-size:12px; color:var(--static); margin-bottom:2px; padding:0">
              Trigger <span style="color:var(--static)"> — a rule fires on an event or on a schedule, never both</span>
            </legend>
            <label style="font-size:13px; margin-right:16px">
              <input type="radio" name="rule-trigger" value="event" bind:group={trigger} /> An event
            </label>
            <label style="font-size:13px">
              <input type="radio" name="rule-trigger" value="schedule" bind:group={trigger} /> A schedule
            </label>
          </fieldset>
          {#if trigger === 'event'}
            <div>
              <div style="font-size:12px; color:var(--static); margin-bottom:2px">
                On event <span style="color:var(--static)"> — the NATS topic this rule listens for</span>
              </div>
              <input
                bind:value={onEvent}
                aria-label="On event"
                placeholder="step.done.*"
                class="mono"
                style="padding:6px; font-size:13px; width:100%"
              />
            </div>
          {:else}
            <!-- The server's RawSchedule: a cadence token, the date its
                 periods count from, and an optional business calendar. -->
            <div style="display:flex; gap:12px; flex-wrap:wrap">
              <div>
                <div style="font-size:12px; color:var(--static); margin-bottom:2px">
                  Cadence <span style="color:var(--static)"> — or every-&lt;n&gt;-minutes</span>
                </div>
                <input
                  bind:value={cadence}
                  aria-label="Cadence"
                  list="rule-cadences"
                  class="mono"
                  style="padding:6px; font-size:13px; width:200px"
                />
                <datalist id="rule-cadences">
                  {#each ['daily', 'weekly', 'biweekly', 'monthly', 'quarterly', 'annually', 'hourly', 'every-5-minutes'] as c (c)}
                    <option value={c}></option>
                  {/each}
                </datalist>
              </div>
              <div>
                <div style="font-size:12px; color:var(--static); margin-bottom:2px">Anchor date</div>
                <input
                  type="date"
                  bind:value={anchorDate}
                  aria-label="Anchor date"
                  class="mono"
                  style="padding:5px; font-size:13px"
                />
              </div>
              <div>
                <div style="font-size:12px; color:var(--static); margin-bottom:2px">
                  Business calendar <span style="color:var(--static)"> — optional</span>
                </div>
                <input
                  bind:value={businessCalendar}
                  aria-label="Business calendar"
                  placeholder="us-banking"
                  class="mono"
                  style="padding:6px; font-size:13px; width:200px"
                />
              </div>
            </div>
          {/if}
          <div>
            <div style="font-size:12px; color:var(--static); margin-bottom:2px">
              When <span style="color:var(--static)"> — optional predicate; rule fires only when it's true</span>
            </div>
            <!-- The live predicates' own shape: 0 of 45 used `==` or
                 `event.` (backlog 3a42c67b, measured 2026-09-27). -->
            <input
              bind:value={whenExpr}
              aria-label="When"
              placeholder={'kind = "ship-a-change" AND outcome = "merged"'}
              class="mono"
              style="padding:6px; font-size:13px; width:100%"
            />
          </div>
          <div>
            <div style="font-size:12px; color:var(--static); margin-bottom:2px">
              Delay <span style="color:var(--static)"> — optional; defers the side-effects (e.g. 5m, 1h)</span>
            </div>
            <input
              bind:value={delay}
              aria-label="Delay"
              placeholder=""
              class="mono"
              style="padding:6px; font-size:13px; width:240px"
            />
          </div>
        </div>
      </Section>

      <!-- do steps -->
      <Section title="Do steps" wide>
        <p class="empty" style="padding:0 0 8px; text-align:left">
          Each step runs a handler with concrete args (string expressions
          evaluated against the event). Steps run in order.
        </p>
        <div style="display:grid; gap:16px; max-width:900px">
          {#each doRows as row, i (i)}
            <div style="border:1px solid var(--hairline); border-radius:6px; padding:12px">
              <div style="display:flex; gap:8px; align-items:center; margin-bottom:8px">
                <span style="font-size:12px; color:var(--static); width:48px">#{i + 1}</span>
                <input
                  bind:value={row.handler}
                  placeholder="handler-name"
                  class="mono"
                  style="padding:6px; font-size:13px; flex:1"
                />
                <button
                  type="button"
                  class="btn"
                  onclick={() => removeDoRow(i)}
                  title="Remove this step"
                >
                  Remove
                </button>
              </div>
              <div style="padding-left:56px; display:grid; gap:6px">
                {#each row.args as arg, ai (ai)}
                  <div style="display:flex; gap:8px; align-items:center">
                    <input
                      bind:value={arg.key}
                      placeholder="arg"
                      class="mono"
                      style="padding:5px; font-size:12px; width:200px"
                    />
                    <span style="color:var(--static)">=</span>
                    <input
                      bind:value={arg.value}
                      placeholder="expression"
                      class="mono"
                      style="padding:5px; font-size:12px; flex:1"
                    />
                    <button
                      type="button"
                      class="btn"
                      onclick={() => removeArg(i, ai)}
                      title="Remove this arg"
                    >
                      ✕
                    </button>
                  </div>
                {/each}
                <div>
                  <button type="button" class="btn" onclick={() => addArg(i)}>+ arg</button>
                </div>
              </div>
            </div>
          {/each}
          <div>
            <button type="button" class="btn" onclick={addDoRow}>+ do step</button>
          </div>
        </div>
      </Section>

      <!-- Version history -->
      <!-- Who declared each version and who drafted, published and
           retired it, served since d8393e48 and shown by nothing until
           backlog 9550ac95; a row opens to its content, so an earlier
           version can be read before it is chosen to roll back to. -->
      <Section title={`Version history (${versions.length})`} wide>
        <table class="data-table data-table-striped version-history">
          <thead>
            <tr>
              <th class="num">Version</th>
              <th>Status</th>
              <th>Source</th>
              <th>By</th>
              <th>Created</th>
              <th></th>
            </tr>
          </thead>
          <tbody>
            {#each versions as v (v.version)}
              {@const open = expanded.includes(v.version)}
              <tr class="version-row" data-version={v.version}>
                <td class="num">{v.version}</td>
                <td>
                  <StatusChip value={v.status} tone={statusTone(v.status)} />
                </td>
                <td class="source mono">{v.source ?? 'product'}</td>
                <td class="by" style="font-size:12px">{authorship(v)}</td>
                <td>{new Date(v.created_at).toISOString().slice(0, 19).replace('T', ' ')}</td>
                <td>
                  <button type="button" class="btn" aria-expanded={open} onclick={() => toggleVersion(v.version)}>
                    {open ? 'Hide' : 'Show'}
                  </button>
                </td>
              </tr>
              {#if open}
                <tr class="version-detail">
                  <td colspan="6">
                    <dl style="display:grid; grid-template-columns:max-content 1fr; gap:2px 12px; margin:0; font-size:12px">
                      <dt style="color:var(--static)">Trigger</dt>
                      <dd class="mono" style="margin:0">{describeTrigger(v)}</dd>
                      <dt style="color:var(--static)">When</dt>
                      <dd class="mono" style="margin:0">{v.when ?? '—'}</dd>
                      <dt style="color:var(--static)">Delay</dt>
                      <dd class="mono" style="margin:0">{v.delay ?? '—'}</dd>
                      <dt style="color:var(--static)">Do</dt>
                      <dd style="margin:0">
                        {#each v.do as d, i (i)}
                          <div class="mono">
                            {d.handler}{#each Object.entries(d.args) as [k, val] (k)}<br />&nbsp;&nbsp;{k} = {val}{/each}
                          </div>
                        {:else}
                          <span>no steps</span>
                        {/each}
                      </dd>
                    </dl>
                  </td>
                </tr>
              {/if}
            {/each}
          </tbody>
        </table>
      </Section>
    </div>
  </div>
{/if}
