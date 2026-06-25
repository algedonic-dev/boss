<script lang="ts">
  // /it — the System Model hub. The IT landing surface: a live-stats
  // launchpad into the surfaces that describe the running state machine.
  // Each card carries one live count (or the event pulse) and links to
  // its surface. Stats load independently and resiliently — a failed
  // source leaves its card a plain link rather than breaking the panel.

  import { onMount } from 'svelte';
  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import Link from '@boss/web-kit/ui/Link.svelte';
  import { href } from '../../router';
  import { SURFACE_CARDS, fmtCount, type StatKind } from './systemModel';

  type LastEvent = { kind: string; source: string; timestamp: string };
  type Stats = {
    jobKinds: number | null;
    openJobs: number | null;
    rules: number | null;
    subjectKinds: number | null;
    classes: number | null;
    stepPlugins: number | null;
    lastEvent: LastEvent | null;
  };
  let stats = $state<Stats>({
    jobKinds: null,
    openJobs: null,
    rules: null,
    subjectKinds: null,
    classes: null,
    stepPlugins: null,
    lastEvent: null,
  });

  async function jset(url: string, apply: (v: unknown) => void): Promise<void> {
    try {
      const r = await fetch(url);
      if (r.ok) apply(await r.json());
    } catch {
      // Leave the stat null; the card renders as a plain link.
    }
  }

  async function loadSubjectsClasses(): Promise<void> {
    try {
      const r = await fetch('/api/subject-kinds');
      if (!r.ok) return;
      const kinds = (await r.json()) as ReadonlyArray<{ kind: string }>;
      stats.subjectKinds = kinds.length;
      // No bare /api/classes (it requires subject_kind) — sum per kind.
      const counts = await Promise.all(
        kinds.map((k) =>
          fetch(`/api/classes?subject_kind=${encodeURIComponent(k.kind)}`)
            .then((cr) => (cr.ok ? cr.json() : []))
            .then((a) => (Array.isArray(a) ? a.length : 0))
            .catch(() => 0),
        ),
      );
      stats.classes = counts.reduce((a, b) => a + b, 0);
    } catch {
      // Leave null.
    }
  }

  onMount(() => {
    void jset('/api/jobs/kinds', (v) => {
      stats.jobKinds = (v as unknown[]).length;
    });
    void jset('/api/jobs/live', (v) => {
      stats.openJobs = (v as { open_total?: number }).open_total ?? 0;
    });
    void jset('/api/dispatcher/rules', (v) => {
      stats.rules = ((v as { rules?: unknown[] }).rules ?? []).length;
    });
    void jset('/api/jobs/step-plugins', (v) => {
      stats.stepPlugins = (v as unknown[]).length;
    });
    void jset('/api/events/tail?limit=5', (v) => {
      const arr = (v as LastEvent[]).filter((e) => e && e.timestamp);
      stats.lastEvent = arr.length
        ? arr.reduce((a, b) => (a.timestamp >= b.timestamp ? a : b))
        : null;
    });
    void loadSubjectsClasses();
  });

  function shortTime(ts: string): string {
    const d = new Date(ts);
    if (Number.isNaN(d.getTime())) return ts;
    return (
      d.toLocaleDateString(undefined, { month: 'short', day: 'numeric' }) +
      ' ' +
      d.toLocaleTimeString(undefined, { hour: '2-digit', minute: '2-digit' })
    );
  }
</script>

{#snippet num(v: number | null)}
  {#if v === null}<span class="sm-dim">…</span>{:else}<span class="sm-num">{fmtCount(v)}</span>{/if}
{/snippet}

{#snippet badge(kind: StatKind)}
  {#if kind === 'jobKinds'}{@render num(stats.jobKinds)}
  {:else if kind === 'openJobs'}{@render num(stats.openJobs)}
  {:else if kind === 'rules'}{@render num(stats.rules)}
  {:else if kind === 'stepPlugins'}{@render num(stats.stepPlugins)}
  {:else if kind === 'subjectsClasses'}
    {#if stats.subjectKinds === null}
      <span class="sm-dim">…</span>
    {:else}
      <span class="sm-num">{stats.subjectKinds}</span>
      <span class="sm-sub"
        >kinds{#if stats.classes !== null} · {fmtCount(stats.classes)} classes{/if}</span
      >
    {/if}
  {:else if kind === 'lastEvent'}
    {#if stats.lastEvent}
      <span class="sm-pulse" title={shortTime(stats.lastEvent.timestamp)}
        >● {stats.lastEvent.kind}</span
      >
    {:else}
      <span class="sm-dim">…</span>
    {/if}
  {/if}
{/snippet}

<div class="sysmodel theme-exec">
  <PageHeader
    eyebrow="Platform · IT"
    title="System Model"
    subtitle="The running state machine — Subjects, Jobs, Steps, Events, and the registries layered over them."
    motif="barrel"
  />

  <div class="sm-grid">
    {#each SURFACE_CARDS as card (card.id)}
      <Link to={href(card.path)} className="sm-card">
        <span class="sm-card-head">
          <span class="sm-card-title">{card.title}</span>
          <span class="sm-card-stat">{@render badge(card.stat)}</span>
        </span>
        <span class="sm-card-blurb">{card.blurb}</span>
        <span class="sm-card-go">Open →</span>
      </Link>
    {/each}
  </div>
</div>

<style>
  .sm-grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(260px, 1fr));
    gap: 16px;
    padding: 0 24px 40px;
  }
  /* Link renders an <a>; lay it out as a card block. */
  .sysmodel :global(a.sm-card) {
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 16px;
    border: 1px solid #e5e7eb;
    border-radius: 10px;
    background: #fff;
    color: inherit;
    text-decoration: none;
    transition:
      border-color 0.12s,
      box-shadow 0.12s,
      transform 0.12s;
  }
  .sysmodel :global(a.sm-card:hover) {
    border-color: var(--brew-amber, #d97706);
    box-shadow: 0 2px 10px rgba(0, 0, 0, 0.06);
    transform: translateY(-1px);
  }
  .sm-card-head {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 12px;
  }
  .sm-card-title {
    font-size: 15px;
    font-weight: 600;
    color: #111827;
  }
  .sm-card-stat {
    display: flex;
    flex-direction: column;
    align-items: flex-end;
    line-height: 1.1;
    text-align: right;
  }
  .sm-num {
    font-size: 24px;
    font-weight: 700;
    color: var(--brew-malt, #b45309);
    font-variant-numeric: tabular-nums;
  }
  .sm-sub {
    font-size: 11px;
    color: #9ca3af;
  }
  .sm-pulse {
    font-size: 12px;
    font-weight: 600;
    color: #16a34a;
    white-space: nowrap;
    max-width: 140px;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .sm-dim {
    font-size: 20px;
    color: #d1d5db;
  }
  .sm-card-blurb {
    font-size: 13px;
    color: #4b5563;
    flex: 1 1 auto;
  }
  .sm-card-go {
    font-size: 12px;
    font-weight: 600;
    color: var(--brew-amber, #d97706);
  }
</style>
