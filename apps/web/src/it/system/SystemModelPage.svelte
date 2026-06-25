<script lang="ts">
  // /system — the System Model perspective root. Renders the live
  // System-Model view (Jobs in flight + the per-JobKind step-graph),
  // the same component the public landing wraps. The remaining
  // model-definition surfaces (Workflows, Dispatcher, Subjects &
  // Classes, Step plugins, Audit Log, Atlas, …) are reached from the
  // System Model sidebar.

  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import SystemModelLiveView from './SystemModelLiveView.svelte';
</script>

<div class="sysmodel theme-exec">
  <PageHeader
    eyebrow="Platform · System Model"
    title="System Model"
    subtitle="The running state machine — Subjects, Jobs, Steps, Events, and the registries layered over them."
    motif="barrel"
  />

  <div class="sm-body">
    <SystemModelLiveView />
  </div>
</div>

<style>
  .sm-body {
    padding: 0 24px 40px;
  }
</style>
