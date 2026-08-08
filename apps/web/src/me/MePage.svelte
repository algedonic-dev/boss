<script lang="ts">
  // Phase-0 port of apps/web/src/me/MePage.tsx.
  //
  // Scope-reduced for the spike: hero + "My Jobs" list + at-a-glance
  // count. Sub-panels (bulletins, next-actions, messages, certs) are
  // deferred to phase 1. The point is to exercise the hot paths —
  // state, $effect, $derived, fetch — not to match feature parity.
  //
  // Conceptual mapping (see docs/design/human-powered-state-machine.md):
  //   $state    → a cell of machine memory
  //   $derived  → a projection of that memory
  //   $effect   → a transition that reads/writes the world
  // This page uses all three.

  import { session } from '@boss/web-kit/session/session.svelte';
  import { appNow } from '@boss/web-kit/sim-clock';
  import { isPending, isInFlight, isTerminal, type StepStatus } from '../jobs/types';
  import { navigate, href } from '../router';
  import { entityHref } from '@boss/web-kit/ui/entity-href';
  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import Section from '@boss/web-kit/ui/Section.svelte';

  type StepSummary = {
    id: string;
    kind: string;
    title: string;
    status: StepStatus;
    assignee_id: string | null;
    sign_offs_required?: string[];
    sort_order: number;
  };

  type JobSummary = {
    id: string;
    kind: string;
    title: string;
    status: string;
    priority: string;
    owner_id: string;
    opened_on: string;
    due_on: string | null;
    steps: StepSummary[];
  };

  // The session rune exposes the current user. We read the id into
  // a local $derived so our fetch effect tracks exactly that
  // dependency. If the user changes (persona switch in phase 1),
  // the effect re-runs.
  let userId = $derived(
    session.value.kind === 'ready' ? session.value.user.id : null,
  );

  let jobs = $state<JobSummary[]>([]);

  /// The step of a Job that YOU can act on right now.
  ///
  /// The page was listing every Job with a step assigned to you in
  /// `isPending || isInFlight` — and `isPending` covers BOTH `pending`
  /// and `ready`. A `pending` step is yours but blocked: its
  /// `ready_when` predicate is not satisfied, so there is nothing to
  /// do. Mixed into one list with work you can actually start, that
  /// reads as "a bunch of random jobs and steps", because half of it
  /// is not addressable and nothing on the card says which half.
  ///
  /// `ready` and `active` are the ones waiting on you.
  function actionableStep(j: JobSummary): StepSummary | null {
    return (
      j.steps?.find(
        (s) =>
          s.assignee_id === userId && (s.status === 'ready' || s.status === 'active'),
      ) ?? null
    );
  }

  /// Jobs you can start now. Blocked ones are still listed — a blocked
  /// step is a commitment and hiding it would move the confusion
  /// rather than remove it — but they sort below, and their card says
  /// so, so the first thing on the page is the first thing to do.
  let readyJobs = $derived(jobs.filter((j) => actionableStep(j) !== null));
  let openJobsCapped = $state(false);
  let openJobsTotal = $state(0);
  let openJobsLoaded = $state(0);
  let loading = $state(true);

  $effect(() => {
    const uid = userId;
    if (!uid) return;
    loading = true;
    let cancelled = false;

    async function load() {
      try {
        const resp = await fetch('/api/jobs?status=open&limit=200');
        if (!resp.ok) {
          if (!cancelled) loading = false;
          return;
        }
        const body = (await resp.json()) as {
          data: JobSummary[];
          total?: number;
        };
        const all = body.data ?? [];
        openJobsLoaded = all.length;
        openJobsTotal =
          typeof body.total === 'number' ? body.total : all.length;
        openJobsCapped = openJobsTotal > all.length;
        const mine = all.filter(j =>
          j.steps?.some(
            s =>
              s.assignee_id === uid &&
              (isPending(s.status) || isInFlight(s.status)),
          ),
        );
        const priority: Record<string, number> = {
          emergency: 0,
          urgent: 1,
          standard: 2,
          scheduled: 3,
        };
        mine.sort((a, b) => {
          // Actionable before blocked, then the existing order. Sorting
          // by priority alone put an urgent BLOCKED step above a
          // standard one you could actually finish.
          const aa = a.steps?.some(
            (s) => s.assignee_id === uid && (s.status === 'ready' || s.status === 'active'),
          )
            ? 0
            : 1;
          const ab = b.steps?.some(
            (s) => s.assignee_id === uid && (s.status === 'ready' || s.status === 'active'),
          )
            ? 0
            : 1;
          if (aa !== ab) return aa - ab;
          const pa = priority[a.priority] ?? 3;
          const pb = priority[b.priority] ?? 3;
          if (pa !== pb) return pa - pb;
          if (a.due_on && b.due_on) return a.due_on.localeCompare(b.due_on);
          if (a.due_on) return -1;
          if (b.due_on) return 1;
          return 0;
        });
        if (!cancelled) {
          jobs = mine;
          loading = false;
        }
      } catch {
        if (!cancelled) loading = false;
      }
    }

    load();
    return () => {
      cancelled = true;
    };
  });

  function timeOfDay(): string {
    const h = new Date().getHours();
    if (h < 12) return 'morning';
    if (h < 17) return 'afternoon';
    return 'evening';
  }

  function tenureYears(hireDate: string): number {
    return (
      (appNow().getTime() - new Date(hireDate).getTime()) /
      (1000 * 60 * 60 * 24 * 365)
    );
  }
</script>

{#if session.value.kind === 'loading'}
  <div class="theme-exec" style="padding: 32px">Loading session…</div>
{:else if session.value.kind === 'unauthenticated'}
  <div class="theme-exec" style="padding: 32px">
    <p class="empty">
      Not signed in. Reload the page to log in.
    </p>
  </div>
{:else if session.value.kind === 'unrecognized'}
  <div class="theme-exec" style="padding: 32px">
    <p class="empty">
      Signed in as <strong>{session.value.username}</strong>, but no
      matching employee in the roster.
    </p>
  </div>
{:else}
  {@const user = session.value.user}
  <div class="theme-exec" style="padding: 0 32px 32px">
    <PageHeader
      eyebrow={`Good ${timeOfDay()}`}
      title={user.name}
      subtitle={`${user.role} · ${tenureYears(user.hire_date).toFixed(1)} years · ${user.department}`}
      motif="glass"
    />

    <div class="me-grid">
      <Section title="My Jobs" wide>
        {#if openJobsCapped}
          <div class="myday-cap-note" role="status">
            Scanned the most recent <strong>{openJobsLoaded.toLocaleString()}</strong>
            of <strong>{openJobsTotal.toLocaleString()}</strong> open jobs for your assignments —
            steps you own on older open jobs may not appear here yet.
          </div>
        {/if}
        {#if loading}
          <div class="myday-loading">Loading jobs…</div>
        {:else if jobs.length === 0}
          <div class="myday-empty">
            No active jobs assigned to you right now.
          </div>
        {:else}
          <div class="myday-jobs-list">
            {#each jobs as job (job.id)}
              {@const mySteps = job.steps.filter(
                s =>
                  s.assignee_id === userId &&
                  (isPending(s.status) || isInFlight(s.status)),
              )}
              {@const total = job.steps.length}
              {@const done = job.steps.filter(
                s => isTerminal(s.status),
              ).length}
              <!-- svelte-ignore a11y_click_events_have_key_events -->
              <!-- svelte-ignore a11y_no_static_element_interactions -->
              <div
                class="myday-job-card"
                onclick={() => navigate(entityHref('job', job.id))}
              >
                <div class="myday-job-header">
                  {#if actionableStep(job)}
                    <span class="myday-step myday-step-ready"
                      >{actionableStep(job)?.title}</span
                    >
                  {:else}
                    <span class="myday-step myday-step-blocked">blocked</span>
                  {/if}
                  <span class="myday-workflow">{job.kind}</span>
                  <span class="myday-job-title">{job.title}</span>
                  {#if job.priority !== 'standard'}
                    <span class="chip chip-sm">{job.priority}</span>
                  {/if}
                </div>
                <div class="myday-job-progress">
                  <div class="myday-progress-bar">
                    <div
                      class="myday-progress-fill"
                      style="width: {total > 0
                        ? (done / total) * 100
                        : 0}%"
                    ></div>
                  </div>
                  <span class="myday-progress-label">
                    {done}/{total} steps
                  </span>
                </div>
                <div class="myday-job-mysteps">
                  {#each mySteps as step (step.id)}
                    <span class="myday-step">→ "{step.title}"</span>
                  {/each}
                </div>
                {#if job.due_on}
                  <div class="myday-job-due">Due {job.due_on}</div>
                {/if}
              </div>
            {/each}
          </div>
        {/if}
      </Section>

      <Section title="At a glance">
        <div class="me-stats">
          <div class="me-stat-card">
            <div class="me-stat-num">{jobs.length}</div>
            <div class="me-stat-label">active jobs</div>
          </div>
        </div>
      </Section>
    </div>
  </div>
{/if}

<style>
  /* The step, not the Job, is what you act on — so it leads the card. */
  .myday-step {
    font-size: 10px;
    text-transform: uppercase;
    letter-spacing: 0.5px;
    font-weight: 600;
    padding: 1px 6px;
    border-radius: 3px;
    white-space: nowrap;
  }
  .myday-step-ready {
    background: #ecfdf5;
    color: #047857;
  }
  .myday-step-blocked {
    background: var(--bg, #f5f5f4);
    color: var(--text-dim, #78716c);
  }

  .myday-cap-note {
    padding: 8px 12px;
    background: #fff7ed;
    border: 1px solid #fdba74;
    border-radius: 6px;
    font-size: 13px;
    color: #7c2d12;
    margin: 0 0 12px 0;
  }
</style>
