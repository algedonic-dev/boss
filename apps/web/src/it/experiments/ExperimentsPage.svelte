<script lang="ts">
  // /system/experiments — placeholder for the coming "Evolve" workflow.
  //
  // The System Model tab is where the model is operated and,
  // increasingly, where it is *changed*. Experiments will host
  // controlled, sandboxed modifications to the running model — a new
  // JobKind version, a Class / Subject-kind tweak, a dispatcher rule,
  // a policy change — trialled safely before they go live. Shipped as
  // a placeholder now (per the IA decision) so the slot is reserved
  // and the intent is visible; the real surface lands when the
  // controlled-modification workflow is built.
  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
</script>

<PageHeader
  eyebrow="System Model · Evolve"
  title="Experiments"
  subtitle="Controlled, reversible modifications to the running model — coming soon."
  motif="barrel"
/>

<div class="exp-body">
  <p class="exp-lead">
    The System Model tab is where the model is <em>operated</em> — and,
    increasingly, where it is <em>changed</em>. Experiments will let you
    propose a controlled modification to the model — a new JobKind
    version, a Class or Subject-kind tweak, a dispatcher rule, a policy
    change — and trial it safely before it goes live.
  </p>

  <ul class="exp-list">
    <li>
      <strong>Sandboxed.</strong> An experiment is a draft change set,
      isolated from the live registries until you promote it.
    </li>
    <li>
      <strong>Observable.</strong> See the projected effect against the
      live event stream before committing anything.
    </li>
    <li>
      <strong>Reversible.</strong> Promote it, or discard it with no
      trace in the system of record.
    </li>
  </ul>

  <p class="exp-note">
    Not built yet — this surface reserves its place in the System Model
    workflow. Open questions are tracked in <strong>Design review</strong>.
  </p>
</div>

<style>
  .exp-body {
    max-width: 72ch;
  }
  .exp-lead {
    font-size: 15px;
    line-height: 1.6;
    color: var(--text, #1c1917);
  }
  .exp-list {
    margin: 18px 0;
    padding-left: 20px;
    line-height: 1.7;
  }
  .exp-list li {
    margin-bottom: 8px;
  }
  .exp-note {
    margin-top: 22px;
    padding: 12px 14px;
    background: var(--brew-amber-bg, #fef3c7);
    border: 1px solid var(--brew-amber, #fcd34d);
    border-radius: 8px;
    font-size: 13px;
    color: var(--brew-malt, #92400e);
  }
</style>
