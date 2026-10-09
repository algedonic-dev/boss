<script lang="ts">
  // Generic step surface — fallback for kinds without a specialised
  // view. Doubles as the "assign tech / reschedule" affordance that
  // every service Job's steps pick up implicitly. Port of
  // apps/web-legacy/src/steps/GenericSurface.tsx.

  import { onDestroy, tick, untrack, type Snippet } from 'svelte';
  import {
    isTerminal as _isTerminal,
    type StepStatus,
    type StepField,
  } from '../jobs/types';
  import type { SpecStep } from '../jobs/fork';
  import type { Employee } from '../people/types';
  import { releaseStep, saveStep, startStep } from './stepWrite';
  import {
    completeWithPresence,
    scrollNote,
    shownAfter,
    signedRows,
    signedText,
    type ShownStep,
  } from './presence';
  import { PROCEDURE_KEY } from './procedure';
  import {
    HOLDER_LOCKED_NOTE,
    askReleaseReason,
    claimedFor,
    gestureFields,
    holderLocked,
    startable,
  } from './holder';
  import { session } from '@boss/web-kit/session/session.svelte';
  import {
    askRoutes,
    completeLabel,
    missingRequired as missingOf,
    needsLine,
    optionsFor,
    specSlugOf,
  } from './stepAsk';

  type StepData = {
    id: string;
    kind: string;
    title: string;
    /// The authored step name inside its Workflow — what the spec's
    /// predicates refer to. On the wire; optional for older callers.
    spec_slug?: string;
    status: StepStatus;
    assignee_id: string | null;
    metadata: Record<string, unknown>;
    notes: string | null;
    /// What the step demands of its completion, when it says: `presence`
    /// is a passkey. A `human_only` step demands one whether it says so
    /// or not (see `passkeyAsked`).
    assurance_required?: string | null;
    /// The step's completion contract. Declared on the Workflow step
    /// (inline authoring), so it is data rather than a bespoke
    /// surface — which is exactly why this generic view can honour it.
    fields?: StepField[];
  };

  type Props = Readonly<{
    step: StepData;
    jobId: string;
    onUpdate: () => void;
    /// The step's case — the decision-context panel — rendered by the
    /// dispatcher INSIDE this card, under the title and above the
    /// form. It used to sit above the card, so the order on screen
    /// was brief, title, assignee, due date, form: the one thing the
    /// step asks for came fifth (feedback 26ae4d44).
    children?: Snippet;
  }>;
  let { step, jobId, onUpdate, children }: Props = $props();

  const initialDueOn =
    typeof step.metadata.due_on === 'string' ? step.metadata.due_on : '';

  /// Values for the step's declared fields, seeded from whatever is
  /// already in metadata.
  ///
  /// Without this the surface could not complete a step that declares
  /// a required field: it sent `status: completed` with the existing
  /// metadata and the API refused it. That made every such step a dead
  /// end everywhere except a bespoke plugin — which defeats inline
  /// field authoring, whose whole point is that a Workflow can state
  /// its own contract without one.
  let fieldValues = $state<Record<string, string>>(
    Object.fromEntries(
      (step.fields ?? []).map((f) => {
        const v = step.metadata[f.name];
        return [f.name, typeof v === 'string' ? v : ''];
      }),
    ),
  );

  let missingRequired = $derived(missingOf(step.fields ?? [], fieldValues));
  let needs = $derived(needsLine(missingRequired));

  /// The Workflow's step graph, read so each enum option can say which
  /// step it opens (stepAsk). Fetched only when the step has an enum
  /// field to explain; the job says which kind and which VERSION the
  /// packet is pinned to, and that version's spec is the one whose
  /// predicates will actually route this answer. A failed read leaves
  /// every route null — the select still shows its words, the button
  /// still says Complete — so the surface degrades, never blanks.
  let specSteps = $state<ReadonlyArray<SpecStep> | null>(null);
  let hasEnumField = $derived((step.fields ?? []).some((f) => optionsFor(f) !== null));
  $effect(() => {
    if (!hasEnumField) return;
    let cancelled = false;
    void (async () => {
      try {
        const jr = await fetch(`/api/jobs/${jobId}`, { headers: { accept: 'application/json' } });
        if (!jr.ok || cancelled) return;
        const job = (await jr.json()) as { kind?: string; workflow_version?: number };
        if (cancelled || !job.kind) return;
        const kind = encodeURIComponent(job.kind);
        const path =
          typeof job.workflow_version === 'number'
            ? `/api/workflows/${kind}/versions/${job.workflow_version}`
            : `/api/workflows/${kind}`;
        const sr = await fetch(path, { headers: { accept: 'application/json' } });
        if (!sr.ok || cancelled) return;
        const spec = (await sr.json()) as { steps?: unknown };
        if (cancelled) return;
        specSteps = Array.isArray(spec.steps) ? (spec.steps as SpecStep[]) : null;
      } catch {
        // No graph is a quiet absence: the words render without routes.
      }
    })();
    return () => {
      cancelled = true;
    };
  });
  let specSlug = $derived(specSlugOf(step, specSteps));
  let routesByField = $derived(
    new Map(
      (step.fields ?? [])
        .filter((f) => optionsFor(f) !== null)
        .map((f) => [f.name, askRoutes(specSteps, specSlug, f)]),
    ),
  );
  /// The fork field — the first enum field — is the one whose chosen
  /// value names the route the Complete control takes.
  let forkField = $derived((step.fields ?? []).find((f) => optionsFor(f) !== null) ?? null);
  let completeText = $derived(
    forkField
      ? completeLabel(routesByField.get(forkField.name) ?? [], fieldValues[forkField.name] ?? '')
      : 'Complete',
  );
  /// The ask renders whenever the step has a contract and a person can
  /// still answer it. `ready` is included: an assigned triage step IS
  /// ready, and the API accepts completion from there (TriageFlow has
  /// completed forks from ready since it existed). Making a person
  /// press Start first, then find Complete, was half of what stopped
  /// David on 2026-09-15.
  let hasAsk = $derived(
    (step.fields ?? []).length > 0 && (step.status === 'ready' || step.status === 'active'),
  );

  let notes = $state(step.notes ?? '');
  let assigneeId = $state(step.assignee_id ?? '');
  let dueOn = $state(initialDueOn);
  let saving = $state(false);
  /// A refused write, rendered inline. The surface keeps its state
  /// exactly as the user left it so the same click can retry; it
  /// never pretends the write landed (packet cc9d7fc6).
  let writeError = $state<string | null>(null);
  $effect(() => {
    // The surface instance is reused when the rail switches steps —
    // an error from step A must not render under step B.
    void step.id;
    writeError = null;
  });
  /// The picker follows the step it shows (backlog 848477c3): the same
  /// reuse carried step A's pick onto step B, and every Save, Start and
  /// Complete sends it. Re-read when the step or its stored holder
  /// changes — the key is a string, so a re-fetch of the same values
  /// does not wipe a pick in progress.
  let holderKey = $derived(`${step.id}\u0000${step.assignee_id ?? ''}`);
  $effect(() => {
    void holderKey;
    assigneeId = untrack(() => step.assignee_id ?? '');
  });
  /// An active step's holder is fixed until it is released (backlogs
  /// 650ebd0c, 0f42efa0), so the picker is not offered there.
  let locked = $derived(holderLocked(step));
  let terminal = $derived(_isTerminal(step.status));

  let employees = $state<Employee[]>([]);

  $effect(() => {
    let cancelled = false;
    (async () => {
      try {
        const r = await fetch('/api/people');
        if (r.ok) {
          const roster = (await r.json()) as Employee[];
          if (!cancelled) employees = roster;
        }
      } catch {
        // ignore
      }
    })();
    return () => {
      cancelled = true;
    };
  });

  let empNames = $derived.by(() => {
    const m = new Map<string, string>();
    for (const e of employees) m.set(e.id, e.name ?? "");
    return m;
  });


  let assigneeDirty = $derived(
    (assigneeId || null) !== (step.assignee_id ?? null),
  );
  let dueOnDirty = $derived((dueOn || '') !== initialDueOn);
  let dirty = $derived(assigneeDirty || dueOnDirty);

  let activeEmployees = $derived(
    [...employees].sort((a, b) => (a.name ?? "").localeCompare(b.name ?? "")),
  );

  // A STEP RESERVED FOR A PERSON COMPLETES ON THEIR PASSKEY (David,
  // 2026-10-07, item 570c66e9: "human-only step completion should use
  // passkey for enforcement"). The jobs API refuses a bare completion of
  // a `human_only` step — the id a caller asserts is not proof of the
  // person — and nine of the ten plain human-only steps on the live
  // registry (a flight's decide and widen, the deposit keys' scope,
  // verify and revoke, a GitHub installation's scope) render HERE, where
  // Complete was one PUT with no ceremony. So on such a step this
  // surface does what the approval surface does: it draws everything the
  // passkey would sign, and answers the server's `{required: "presence"}`
  // with ONE tap on the step as shown and ONE retry (presence.ts).
  //
  // The declaration is read the way the server reads it
  // (boss-jobs human_only::declared): the bool `true` or the string
  // "true". A step that declares presence itself is drawn the same way.
  let passkeyAsked = $derived(
    !_isTerminal(step.status) &&
      (step.metadata['human_only'] === true ||
        (typeof step.metadata['human_only'] === 'string' &&
          step.metadata['human_only'].trim().toLowerCase() === 'true') ||
        step.assurance_required === 'presence'),
  );
  // WHAT THE PASSKEY SIGNS IS ON SCREEN (design f623e425 D3): the step's
  // title and EVERY metadata key, with this gesture's own write folded
  // in while it is in flight — the one object the ceremony is handed as
  // its answer to "what is on screen", so it refuses any key it would
  // sign that is not drawn (presence.ts notShown).
  let pending = $state<Readonly<Record<string, unknown>> | null>(null);
  let onScreen = $derived<ShownStep>({
    title: step.title,
    metadata: shownAfter(step.metadata, pending ?? {}),
  });
  let signed = $derived(signedRows(onScreen));
  // A destroyed surface, or one the rail has moved to another step,
  // shows nothing of the step a gesture began on (backlogs d82b5f60,
  // 7c53b1bf): its ceremony refuses and its prompt is taken down.
  let destroyed = false;
  let gesture: AbortController | null = null;
  onDestroy(() => {
    destroyed = true;
    gesture?.abort();
  });
  let scrolling = $state<Readonly<Record<string, boolean>>>({});
  let shownStepId = $derived(step.id);
  $effect(() => {
    void shownStepId;
    gesture?.abort();
    gesture = null;
    pending = null;
    scrolling = {};
  });
  function watchOverflow(node: HTMLElement, row: { key: string; text: string }) {
    let key = row.key;
    const check = (): void => {
      const scrolls =
        node.scrollHeight > node.clientHeight + 1 || node.scrollWidth > node.clientWidth + 1;
      if ((scrolling[key] ?? false) !== scrolls) scrolling = { ...scrolling, [key]: scrolls };
    };
    const observer = typeof ResizeObserver === 'function' ? new ResizeObserver(check) : null;
    observer?.observe(node);
    check();
    return {
      update(next: { key: string; text: string }) {
        key = next.key;
        requestAnimationFrame(check);
      },
      destroy() {
        observer?.disconnect();
      },
    };
  }

  /// Complete a step that asks for a passkey. Three writes, in the order
  /// the server needs them: the answer through the merge door and the
  /// notes and holder through a PUT that does NOT complete — a
  /// completion that stands on a passkey may change nothing beside the
  /// status (backlogs c0b56fd9, 42e7c6b9) — then the completion, bare,
  /// which the server answers by asking for the tap.
  async function completeWithPasskey(
    metadata: Readonly<Record<string, unknown>>,
  ): Promise<void> {
    // The step this click was aimed at, captured before the first await
    // and the only step any write below names (d82b5f60).
    const target = { id: step.id, title: step.title, metadata: { ...step.metadata } };
    const job = jobId;
    const controller = new AbortController();
    gesture = controller;
    try {
      // Drawn before anything is written or signed (D3).
      pending = metadata;
      await tick();
      const details = { notes: notes ?? undefined, ...gestureFields(step, assigneeId) };
      const wrote = await saveStep(job, target.id, { ...details, metadata });
      if (wrote.kind === 'failed') {
        writeError = wrote.error;
        return;
      }
      const done = await completeWithPresence(
        job,
        target.id,
        { title: target.title, metadata: shownAfter(target.metadata, metadata) },
        () => (!destroyed && step.id === target.id && passkeyAsked ? onScreen : null),
        controller.signal,
      );
      if (done.kind === 'failed') writeError = done.error;
      // The answer DID land either way: the host re-reads the step.
      onUpdate();
    } finally {
      pending = null;
      if (gesture === controller) gesture = null;
    }
  }

  /// `status` is the one the gesture moves the step to — Complete — and
  /// absent for a Save: the page's own copy of the status is a snapshot,
  /// and sending it back released an agent's claim made after the page
  /// was drawn (backlog 6ef4a36b). A Start sends no status either: it
  /// saves what the page holds and then claims through the claim door,
  /// for the holder the picker shows (design 611fbffd, clause b).
  async function persist(status?: string, start = false): Promise<void> {
    saving = true;
    writeError = null;
    try {
      // Only the keys this surface owns go to the merge door — never a
      // spread of the step's metadata (backlog e39a9d2a). A cleared due
      // date is an explicit null, which the door deletes; it used to be
      // cleared by OMISSION from a wholesale PUT.
      const metadata = {
        ...(dueOnDirty ? { due_on: dueOn || null } : {}),
        // Only send fields the operator actually filled — an
        // empty string is not an answer, and writing one would
        // satisfy a required-field check with nothing in it.
        ...Object.fromEntries(
          Object.entries(fieldValues).filter(([, v]) => v.trim() !== ''),
        ),
      };
      if (status === 'completed' && passkeyAsked) {
        await completeWithPasskey(metadata);
        return;
      }
      const body = {
        notes: notes ?? undefined,
        ...gestureFields(step, assigneeId, status),
        metadata,
      };
      let res = await saveStep(jobId, step.id, body);
      if (res.kind === 'ok' && start) {
        res = await startStep(jobId, step.id, claimedFor(assigneeId));
      }
      if (res.kind === 'failed') {
        writeError = res.error;
        return;
      }
      onUpdate();
    } finally {
      saving = false;
    }
  }

  /// Hand an active step back: `ready`, nobody's, for the next holder
  /// to claim (backlog 6ef4a36b — the note beside the picker said a
  /// held step changes hands "by release", and the page had none). It
  /// asks why first and records the answer, as `boss step release`
  /// does; a partial release stays on screen rather than refreshing it
  /// away (the review of car 675f1858, #2).
  async function release(): Promise<void> {
    const why = askReleaseReason();
    if (why === null) return;
    saving = true;
    writeError = null;
    try {
      const by = session.value.kind === 'ready' ? session.value.user.id : null;
      const res = await releaseStep(jobId, step, why, by);
      if (res.kind !== 'ok') {
        writeError = res.error;
        return;
      }
      onUpdate();
    } finally {
      saving = false;
    }
  }

  /// System/structured leftovers only — human-written string context
  /// renders as prose via contextEntries; declared fields render as
  /// the form. What remains (objects, flags) shows as a small dump.
  let extraMetadataEntries = $derived.by(() => {
    const declared = new Set((step.fields ?? []).map((f) => f.name));
    return Object.entries(step.metadata ?? {}).filter(
      ([k, v]) =>
        !declared.has(k) &&
        !HIDDEN_KEYS.has(k) &&
        !(typeof v === 'string' && v.trim().length > 0),
    );
  });

  /// Undeclared string metadata is CONTEXT someone wrote for the
  /// operator (a decision brief, options, an agent's analysis) — it
  /// was invisible because only declared fields render, which turned
  /// context-rich steps into bare forms. Internal routing keys stay
  /// hidden.
  const HIDDEN_KEYS = new Set([
    'authority_role', 'due_on', 'notify_on_done', 'trigger_kind', 'trigger_name',
    // The decision panel (DecisionContext, mounted by StepSurface
    // above every platform surface) already renders context_md as the
    // step's brief. Re-dumping it here printed the same brief twice on
    // one screen — the second time as a flattened raw-markdown wall
    // (browser-verified on the live gateway, 2026-08-19).
    'context_md',
    // The step's procedure, for the same reason one line up: StepProcedure
    // (mounted by StepSurface above every surface) renders it as the
    // step's instructions, with its paragraphs intact. This list is
    // where it used to land — unstyled prose under its own lowercase
    // key, below the assignee and due-date rows, newlines collapsed
    // into one wall. The key is imported so the panel and this
    // exclusion cannot disagree about its name (CLAUDE.md §9a).
    PROCEDURE_KEY,
  ]);
  let contextEntries = $derived.by(() => {
    const declared = new Set((step.fields ?? []).map((f) => f.name));
    return Object.entries(step.metadata ?? {})
      .filter(([k, v]) =>
        !declared.has(k) && !HIDDEN_KEYS.has(k) &&
        typeof v === 'string' && v.trim().length > 0)
      .map(([k, v]) => ({ key: k.replaceAll('_', ' '), value: v as string }));
  });
</script>

<div class="step-surface step-generic">
  <div class="step-surface-header">
    <h3>{step.title}</h3>
    <span class="step-kind-label">{step.kind}</span>
    <span class="step-status step-status-{step.status}">{step.status}</span>
  </div>

  <!-- The one ask, in reading order: the step's title (its question),
       the case for it (the dispatcher's decision-context panel, passed
       in as children), then the answer — the declared fields and the
       control that records them. Nothing else sits between. The
       assignee, due date and notes are details of the step, and they
       follow the ask (feedback 26ae4d44). -->
  {@render children?.()}

  {#if passkeyAsked}
    <!-- A step reserved for a person completes on their passkey, and a
         passkey signs the step's title and every metadata key: all of it
         is drawn here, as the bytes it is, before the tap is asked. -->
    <section class="step-signed-keys" aria-label="What your passkey signs">
      <div class="step-signed-keys-head">What your passkey signs</div>
      <p class="step-signed-keys-note">
        Completing this step takes your passkey. It signs the step
        <strong class="step-signed-title">{signedText(onScreen.title)}</strong>
        and every key below, exactly as shown. Text in double quotes has each
        character you could not otherwise see or tell apart written as an
        escape. What you enter below joins them when you press Complete.
      </p>
      <dl>
        {#each signed as row (row.key)}
          <dt class="step-signed-key">{row.label}</dt>
          <dd>
            <pre class="step-signed-value" use:watchOverflow={{ key: row.key, text: row.text }}>{row.text}</pre>
            {#if scrolling[row.key]}
              <div class="step-signed-overflow">{scrollNote(row.text)}</div>
            {/if}
          </dd>
        {/each}
      </dl>
    </section>
  {/if}

  {#if hasAsk}
    <!-- The step's own completion contract, rendered from data.
         Validators run at `completed`, so a required field missing
         here is not a warning — it is a step that cannot close.
         Independent of any metadata: the form's presence depends on
         the CONTRACT, not on whether context happens to exist (they
         were tangled, and a context-less step lost its form). -->
    <div class="step-fields step-ask">
      {#each step.fields ?? [] as f (f.name)}
        {@const options = optionsFor(f)}
        {@const routes = routesByField.get(f.name) ?? []}
        <label class="step-field">
          <span class="step-field-label">
            {f.name.replace(/_/g, ' ')}{#if f.required}<span
                class="step-field-required"
                title="required">*</span
              >{/if}
          </span>
          {#if options}
            <!-- Each option carries the step it opens, read off the
                 Workflow's predicates: "build — Build the change". The
                 bare word was the whole label before, and six bare
                 words are not a choice a person can make unaided. -->
            <select class="step-field-input" bind:value={fieldValues[f.name]}>
              <option value="">Choose…</option>
              {#each routes as r (r.value)}
                <option value={r.value}>{r.route ? `${r.value} — ${r.route}` : r.value}</option>
              {/each}
            </select>
          {:else if f.field_type === 'string'}
            <!-- Free text is prose (evidence, a finding, a brief), and a
                 one-line box crammed a whole markdown brief into one
                 scrolling line. -->
            <textarea class="step-field-input" rows="2" bind:value={fieldValues[f.name]}
            ></textarea>
          {:else}
            <input
              class="step-field-input"
              type="text"
              bind:value={fieldValues[f.name]}
              placeholder={f.field_type}
            />
          {/if}
        </label>
      {/each}
      <div class="step-ask-actions">
        <!-- Labelled with its effect — the route the chosen answer
             takes — and, when it cannot be pressed, the field it is
             waiting on, in the row rather than a hover title. -->
        <button
          class="btn btn-primary"
          onclick={() => persist('completed')}
          disabled={saving || missingRequired.length > 0}
        >
          {completeText}
        </button>
        {#if needs}
          <span class="step-ask-needs">{needs}</span>
        {:else}
          <span class="step-ask-needs step-ask-legend">* required</span>
        {/if}
      </div>
    </div>
  {/if}

  <div class="step-field step-assign-row">
    <label for={`assignee-${step.id}`}>Assignee</label>
    <select
      id={`assignee-${step.id}`}
      bind:value={assigneeId}
      disabled={terminal || saving || locked}
    >
      <option value="">— unassigned —</option>
      {#each activeEmployees as e (e.id)}
        <option value={e.id}>{e.name} · {e.role}</option>
      {/each}
    </select>
    {#if step.assignee_id && !assigneeDirty}
      <span class="step-meta-row small">
        ({empNames.get(step.assignee_id) ?? step.assignee_id})
      </span>
    {/if}
    {#if locked}
      <span class="step-meta-row small step-holder-locked">{HOLDER_LOCKED_NOTE}</span>
    {/if}
  </div>

  <div class="step-field step-assign-row">
    <label for={`due-${step.id}`}>Due on</label>
    <input
      id={`due-${step.id}`}
      type="date"
      bind:value={dueOn}
      disabled={terminal || saving}
    />
  </div>

  {#if contextEntries.length > 0}
    <!-- Human-written context (a decision brief, options, an agent's
         analysis). This existed in metadata and never rendered — the
         step page was "start buttons with no context" (2026-08-10). -->
    <div class="gs-context">
      {#each contextEntries as c (c.key)}
        <div class="gs-context-item">
          <span class="gs-context-k">{c.key}</span>
          <p class="gs-context-v">{c.value}</p>
        </div>
      {/each}
    </div>
  {/if}

  {#if extraMetadataEntries.length > 0}
    <div class="step-metadata-display">
      {#each extraMetadataEntries as [k, v] (k)}
        <div class="step-meta-row">
          <strong>{k}:</strong>
          {typeof v === 'object' ? JSON.stringify(v) : String(v)}
        </div>
      {/each}
    </div>
  {/if}

  <div class="step-field">
    <label for={`notes-${step.id}`}>Notes</label>
    <textarea
      id={`notes-${step.id}`}
      rows="2"
      bind:value={notes}
      placeholder="Add notes..."
      disabled={terminal}
    ></textarea>
  </div>

  {#if writeError}
    <p class="step-write-error" role="alert">{writeError}</p>
  {/if}

  <div class="step-actions">
    {#if dirty && !terminal}
      <button
        class="btn"
        onclick={() => persist()}
        disabled={saving}
      >
        {saving ? 'Saving…' : 'Save assignment'}
      </button>
    {/if}
    {#if startable(step)}
      <button
        class="btn btn-primary"
        onclick={() => persist(undefined, true)}
        disabled={saving}
      >
        Start
      </button>
    {/if}
    {#if locked}
      <!-- The release the note beside the picker names: the step goes
           back to ready, nobody's, and the next holder claims it. The
           server's policy decides who may; the page is behind the
           write gate like every step surface. -->
      <button
        class="btn"
        onclick={release}
        disabled={saving}
        title="Hand this step back — it returns to ready, and the next holder claims it"
      >
        Release
      </button>
    {/if}
    {#if !terminal && step.status === 'active' && !hasAsk}
      <!-- A step with no contract completes here; one WITH a contract
           completes from the ask above, where the answer is. -->
      <button
        class="btn btn-primary"
        onclick={() => persist('completed')}
        disabled={saving}
      >
        Complete
      </button>
    {/if}
  </div>
</div>

<style>
  .step-ask {
    border: 1px solid var(--border);
    border-left: 3px solid var(--accent);
    border-radius: 6px;
    padding: 10px 12px;
    margin-bottom: 12px;
  }
  .step-ask-actions {
    display: flex;
    align-items: center;
    gap: 10px;
    flex-wrap: wrap;
    margin-top: 8px;
  }
  .step-ask-needs {
    font-size: 12px;
    color: var(--text-dim);
  }
  /* Enamel's field label, the board's `.field label` (backlog 6f471ff6). */
  .step-field-label {
    font-size: 13.5px;
    font-weight: 700;
    color: var(--text);
  }
  .step-ask-legend {
    margin-left: auto;
  }
  .step-field-required {
    color: var(--err);
    margin-left: 2px;
  }
</style>
