<script lang="ts">
  import { href } from '../../router';
  import { formatRelative } from '@boss/web-kit/ui/date';
  import { CAR_WINDOW, gateActivity, type AgentRun, type GateRun } from './crew';
  import type { Remote } from '../../data/remote';
  let { runs, now, gates }: Readonly<{ runs: ReadonlyArray<AgentRun>; now: Date; gates: Remote<ReadonlyArray<GateRun>> }> = $props();
  type RecordReading = Readonly<{ title: string; phase: string; evidence: string; at: string | null }>;
  let readings = $state<Readonly<Record<string, RecordReading | null>>>({});
  let generation = 0;
  function obj(value: unknown): Record<string, unknown> {
    if (!value || typeof value !== 'object' || Array.isArray(value)) throw new Error('Invalid job record');
    return value as Record<string, unknown>;
  }
  function text(value: unknown): string | null { return typeof value === 'string' && value.trim() ? value : null; }
  $effect(() => {
    const packets = [...new Set(runs.flatMap((run) => run.packet ? [run.packet] : []))];
    const thisGeneration = ++generation;
    readings = {};
    void Promise.all(packets.map(async (id) => {
      let reading: RecordReading | null = null;
      try {
        const response = await fetch(`/api/jobs/${encodeURIComponent(id)}`);
        if (!response.ok) throw new Error('Job read failed');
        const job = obj(await response.json());
        if (job.id !== id || !Array.isArray(job.steps)) throw new Error('Wrong job record');
        const steps = job.steps.map(obj);
        const active = steps.find((step) => step.status === 'active');
        const latest = steps.filter((step) => step.status === 'completed' && text(step.completed_by) && typeof step.completed_at === 'string' && Number.isFinite(Date.parse(step.completed_at)))
          .sort((a, b) => Date.parse(String(b.completed_at)) - Date.parse(String(a.completed_at)))[0];
        reading = { title: text(job.title) ?? id, phase: active ? `Job's active step: ${text(active.title) ?? 'unnamed step'}` : 'No active job step recorded', evidence: latest ? `${text(latest.title) ?? 'Step'} completed by ${text(latest.completed_by)}` : 'No attributable completion recorded', at: latest ? text(latest.completed_at) : null };
      } catch { /* A failed reading stays visible on its workstation. */ }
      if (generation === thisGeneration) readings = { ...readings, [id]: reading };
    }));
  });
</script>

<section aria-label="Recorded workstations">
  <header><h2>On the floor now</h2><p>Latest {CAR_WINDOW} open workflow runs, not a complete worker census · read <time datetime={now.toISOString()}>{now.toLocaleTimeString()}</time>. Gate labels describe recorded status at this reading; runtime observations appear separately below.</p></header>
  {#if runs.length === 0}<p>No open run in the latest workflow window. This does not establish an idle runtime.</p>{/if}
  <div class="workstations">
    {#each runs as run (run.id)}
      {@const reading = run.packet ? readings[run.packet] : undefined}
      {@const gate = gates.kind === 'ready' ? gateActivity(run.id, gates.data) : null}
      <article>
        <div class="worker">{run.agent ?? 'Worker identity not recorded'}<span class="lamp" aria-hidden="true"></span></div>
        <h3>{#if run.packet}<a href={href(`/ux/jobs/${run.packet}`)}>{reading?.title ?? run.title}</a>{:else}{run.title}{/if}</h3>
        <p class="standing">{run.at ?? 'Run phase not recorded'}</p>
        <p>{reading?.phase ?? (reading === null ? 'Job evidence unavailable' : run.packet ? 'Reading job evidence…' : 'Job not recorded')}</p>
        {#if gate}
          <p><a href={href(`/ux/jobs/${gate.gateId}`)}>Gate {gate.label}</a>{#if gate.at} · <time datetime={gate.at}>{formatRelative(gate.at, now)}</time>{:else} · event time not recorded{/if}</p>
          {#if gate.label === 'Running'}<p>Waiting for a recorded gate verdict.</p>{/if}
        {:else if gates.kind === 'failed'}
          <p class="unknown">Gate activity unknown: {gates.error}</p>
        {:else if gates.kind === 'loading'}
          <p class="unknown">Reading gate activity…</p>
        {:else}
          <p class="unknown">No gate linked to this run in the latest {CAR_WINDOW} records.</p>
        {/if}
        <p class="unknown">Runtime activity unknown · waiting reason and next action not recorded</p>
        <div class="evidence"><span>Latest job evidence</span><p>{reading?.evidence ?? 'Unknown'}{#if reading?.at}<br /><time datetime={reading.at}>{formatRelative(reading.at, now)}</time>{/if}</p></div>
        <a href={href(`/ux/jobs/${run.id}`)}>Run record</a>
      </article>
    {/each}
  </div>
</section>

<style>
  section { margin: 1rem 0 2rem; }
  header p, .unknown, .evidence { color: var(--static); font-size: .78rem; }
  h2 { font-size: 1.3rem; }
  .workstations { display: grid; grid-template-columns: repeat(auto-fit, minmax(min(100%, 19rem), 1fr)); gap: 1rem; }
  article { border: 1px solid var(--border); border-top: 3px solid var(--signal); background: var(--ink-raised); padding: 1rem; min-width: 0; overflow-wrap: anywhere; }
  .worker { display: flex; justify-content: space-between; font-size: .8rem; color: var(--signal); }
  .lamp { width: .5rem; height: .5rem; border: 1px solid var(--signal); border-radius: 50%; }
  h3 { font-size: 1.05rem; line-height: 1.4; margin: .65rem 0; }
  p { font-size: .82rem; line-height: 1.5; }
  .standing { font-weight: 600; }
  .evidence { border-top: 1px solid var(--border); padding-top: .75rem; margin-top: 1rem; }
  .evidence span { font-size: .7rem; text-transform: uppercase; letter-spacing: .07em; }
  article > a { font-size: .78rem; }
</style>
