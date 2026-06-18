<script lang="ts">
  // Step dispatcher — mounts the surface the StepType REGISTRY names
  // for this step's kind (docs/architecture-decisions.md §Step UX &
  // frontend). The kind → surface mapping is
  // registry data served by /api/jobs/step-types; this file holds the
  // surface-id → component table for the platform-shipped surfaces.
  // Precedence: tenant StepPlugin (if one is registered for the kind)
  // → platform surface named by the registry → GenericSurface (the
  // universal fields/notes card — also the loading/unknown fallback).
  //
  // There is deliberately no kind match here — the
  // no-step-kind-match lint fails the build if one returns.

  import GenericSurface from './GenericSurface.svelte';
  import ApprovalSurface from './ApprovalSurface.svelte';
  import RepairSurface from './RepairSurface.svelte';
  import InspectionSurface from './InspectionSurface.svelte';
  import BillingSurface from './BillingSurface.svelte';
  import IntakeSurface from './IntakeSurface.svelte';
  import ShipmentSurface from './ShipmentSurface.svelte';
  import SchedulingSurface from './SchedulingSurface.svelte';
  import ProductionConsumeSurface from './ProductionConsumeSurface.svelte';
  import HandoffSurface from './HandoffSurface.svelte';
  import ReceivingSurface from './ReceivingSurface.svelte';
  import ProcurementSurface from './ProcurementSurface.svelte';
  import StepPluginMount from './StepPluginMount.svelte';
  import { hasActivePluginFor } from './pluginHost';
  import { surfaceOf } from './surfaceRegistry.svelte';
  import { session } from '../session/session.svelte';
  import FileAttachments from '../content/FileAttachments.svelte';
  import type { StepStatus } from '../jobs/types';

  type StepData = {
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
  };

  type Props = {
    step: StepData;
    jobId: string;
    onUpdate: () => void;
  };
  let { step, jobId, onUpdate }: Props = $props();

  // Async-resolved: does the boss-jobs step-plugin registry have
  // an active row for this kind? Until the fetch returns we
  // render GenericSurface; if a plugin IS registered, we swap to
  // StepPluginMount once the result lands.
  let pluginAvailable = $state<boolean | null>(null);
  $effect(() => {
    pluginAvailable = null;
    let cancelled = false;
    hasActivePluginFor(step.kind).then((avail) => {
      if (!cancelled) pluginAvailable = avail;
    });
    return () => {
      cancelled = true;
    };
  });

  let user = $derived(
    session.value.kind === 'ready'
      ? { id: session.value.user.id, role: session.value.user.role }
      : undefined,
  );
</script>

{#if pluginAvailable === true}
  <StepPluginMount
    kind={step.kind}
    {step}
    {jobId}
    {onUpdate}
    currentUser={user}
  />
{:else if surfaceOf(step.kind) === 'approval'}
  <ApprovalSurface {step} {jobId} {onUpdate} />
{:else if surfaceOf(step.kind) === 'repair'}
  <RepairSurface {step} {jobId} {onUpdate} />
{:else if surfaceOf(step.kind) === 'inspection'}
  <InspectionSurface {step} {jobId} {onUpdate} />
{:else if surfaceOf(step.kind) === 'billing'}
  <BillingSurface {step} {jobId} {onUpdate} />
{:else if surfaceOf(step.kind) === 'intake'}
  <IntakeSurface {step} {jobId} {onUpdate} />
{:else if surfaceOf(step.kind) === 'shipment'}
  <ShipmentSurface {step} {jobId} {onUpdate} />
{:else if surfaceOf(step.kind) === 'scheduling'}
  <SchedulingSurface {step} {jobId} {onUpdate} />
{:else if surfaceOf(step.kind) === 'production-consume'}
  <ProductionConsumeSurface {step} {jobId} {onUpdate} />
{:else if surfaceOf(step.kind) === 'handoff'}
  <HandoffSurface {step} {jobId} {onUpdate} />
{:else if surfaceOf(step.kind) === 'receiving'}
  <ReceivingSurface {step} {jobId} {onUpdate} />
{:else if surfaceOf(step.kind) === 'procurement'}
  <ProcurementSurface {step} {jobId} {onUpdate} />
{:else}
  <GenericSurface {step} {jobId} {onUpdate} />
{/if}

<!--
  Attachments slot — same component every step kind gets, regardless
  of which surface above rendered. Files are a column on every
  primitive, not a per-kind affordance (docs/architecture-decisions.md
  §Content, files, knowledge).
  Lives below the dispatched surface so it doesn't compete with the
  step's primary controls; collapsed empty when there are no files.
-->
<div class="step-attachments">
  <FileAttachments targetKind="step" targetId={step.id} />
</div>

<style>
  .step-attachments {
    margin-top: 12px;
    padding: 8px 12px;
    border-top: 1px dashed var(--border);
  }
</style>
