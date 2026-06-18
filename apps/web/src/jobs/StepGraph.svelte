<script lang="ts">
  // v2 step view for the Job detail page. Renders the step DAG with
  // live status via the shared StepDag component; the selected step's
  // full StepSurface shows below. v2 has no tiers — the graph's edges
  // come from each step's resolved upstream dependencies (`blocked_by`),
  // not a `sort_order` tier bucket.

  import StepSurface from '../steps/StepSurface.svelte';
  import StepDag, { type DagNode, type DagEdge } from './StepDag.svelte';
  import type { StepStatus } from './types';

  // Matches StepSurface's StepData shape so the same object passes
  // through cleanly when a node is selected.
  type Step = {
    id: string;
    kind: string;
    title: string;
    status: StepStatus;
    assignee_id: string | null;
    sort_order: number;
    sign_offs_required?: string[];
    sign_offs?: {
      authority_id: string;
      role: string;
      stamped_at: string;
      shape_hash: string;
    }[];
    metadata: Record<string, unknown>;
    notes: string | null;
    blocked_by?: string[];
  };

  type Props = {
    steps: Step[];
    jobId: string;
    onUpdate: () => void;
  };
  let { steps, jobId, onUpdate }: Props = $props();

  let pickedId = $state<string | null>(null);

  // Resolved selection: the explicitly-clicked step, else the natural
  // focus — the in-flight step, then the next ready one, then the first.
  let selected = $derived.by(() => {
    const explicit = steps.find((s) => s.id === pickedId);
    if (explicit) return explicit;
    return (
      steps.find((s) => s.status === 'active') ??
      steps.find((s) => s.status === 'ready') ??
      steps[0] ??
      null
    );
  });

  let nodes: DagNode[] = $derived(
    [...steps]
      .sort((a, b) => (a.sort_order ?? 0) - (b.sort_order ?? 0))
      .map((s) => ({ id: s.id, title: s.title, kind: s.kind, status: s.status })),
  );

  // Edges from resolved upstream deps. Filter to declared steps so a
  // dangling blocker id can't draw an edge to nowhere.
  let edges: DagEdge[] = $derived(
    steps.flatMap((s) =>
      (s.blocked_by ?? [])
        .filter((b) => steps.some((x) => x.id === b))
        .map((b) => ({ from: b, to: s.id })),
    ),
  );
</script>

<div class="sg">
  {#if steps.length > 0}
    <StepDag
      {nodes}
      {edges}
      selectedId={selected?.id ?? null}
      onNodeClick={(id) => (pickedId = id)}
    />
  {/if}
  {#if selected}
    <div class="sg-detail">
      <StepSurface step={selected} {jobId} {onUpdate} />
    </div>
  {/if}
</div>

<style>
  .sg {
    display: flex;
    flex-direction: column;
    gap: 14px;
  }
  .sg-detail {
    border: 1px solid var(--color-border, #e3d9c4);
    border-radius: 8px;
    background: var(--color-surface-alt, #fafafa);
    padding: 12px 14px;
  }
</style>
