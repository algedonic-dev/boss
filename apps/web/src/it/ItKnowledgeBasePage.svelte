<script lang="ts">
  // /it/kb — IT Knowledge Base.
  //
  // The IT department's reference: the four architecture diagrams,
  // source-derived (Mermaid SVGs from docs/architecture/) so the
  // page doesn't drift the way a hardcoded hosts / stack /
  // providers table would. A prior iteration of this page carried
  // inline tables for those — they went out of alignment with
  // reality the moment any of the underlying state changed, so we
  // deleted them rather than maintain them by hand. The decision
  // record itself lives in the repo as
  // docs/architecture-decisions.md (one consolidated current-truth
  // document), not as an in-app catalog.
  // See `crates/core/boss-core/src/hosts.rs` for the operator host
  // registry (empty by design in OSS — operators name their own
  // hosts via `~/.config/boss/hosts.toml`).

  import Breadcrumb from '@boss/web-kit/ui/Breadcrumb.svelte';
  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import { href } from '../router';

  // Diagrams under it/kb-assets/ — the rendered Mermaid output
  // colocated with the page that consumes them. Regenerate via
  // `infra/architecture/regenerate.sh` (or follow
  // docs/architecture-diagram.md) and copy the SVGs back into
  // kb-assets/.
  import stateSurfacesWorkSvg from './kb-assets/00-state-surfaces-work.svg';
  import primitivesSvg from './kb-assets/01-primitives.svg';
  import serviceMapSvg from './kb-assets/02-service-map.svg';
  import deploymentSvg from './kb-assets/03-deployment.svg';
  import { safeLinkHref } from '@boss/web-kit/links';
</script>

{#snippet diagram(src: string, alt: string)}
  <div class="arch-diagram">
    <img
      src={safeLinkHref(src)}
      {alt}
      style="display:block; margin:0 auto; width:max(100%, 1600px); height:auto"
    />
  </div>
  <div style="font-size:12px; color:var(--static); margin-top:6px; text-align:right">
    <a href={safeLinkHref(src)} target="_blank" rel="noopener noreferrer">Open at full size ↗</a>
  </div>
{/snippet}

<style>
  .layers {
    display: flex;
    flex-direction: column;
    gap: 8px;
    margin-bottom: 16px;
  }
  /* Stacked bands, substrate at the bottom — the diagram IS the
     ordering, so it is CSS rather than a generated asset that could
     drift from the prose beside it. */
  .layer {
    display: flex;
    gap: 14px;
    align-items: flex-start;
    border: 1px solid var(--hairline);
    border-radius: 8px;
    padding: 14px 16px;
    background: var(--ink);
  }
  .layer p {
    margin: 4px 0 0;
    font-size: 14px;
    line-height: 1.55;
    color: var(--static);
  }
  .layer strong { font-size: 15px; color: var(--fog); }
  .layer-n {
    flex: 0 0 28px;
    height: 28px;
    border-radius: 6px;
    display: grid;
    place-items: center;
    font-size: 13px;
    font-weight: 600;
    color: var(--fog);
  }
  .layer-actors   { border-left: 4px solid var(--hairline); }
  .layer-actors   .layer-n { background: var(--ink-raised); }
  .layer-protocols { border-left: 4px solid var(--border-strong); }
  .layer-protocols .layer-n { background: var(--ink-raised); }
  .layer-network  { border-left: 4px solid var(--border-strong); }
  .layer-network  .layer-n { background: var(--ink-raised); }

  .arch-diagram {
    background: var(--ink);
    border: 1px solid var(--hairline);
    border-radius: 8px;
    padding: 16px;
    overflow: auto;
    max-height: 75vh;
  }
</style>

<div class="catalog theme-it">
  <!-- A breadcrumb names the page's parent, and this page's parent is
       the IT department — the words MapPage's own crumb uses (846f0b51).
       The eyebrow says IT for the same reason: /system became /it on
       2026-08-31 (839a7f0f). -->
  <Breadcrumb to={href('/it')}>← The IT world</Breadcrumb>

  <PageHeader
    eyebrow="IT · Knowledge Base"
    title="Knowledge Base"
    subtitle="The reading frame, then the four architecture diagrams — the reference for how BOSS is put together"
  />

  <div style="background:var(--signal-wash); border:1px solid var(--signal); border-radius:8px; padding:14px 16px; margin-bottom:16px; font-size:14px; line-height:1.55; color:var(--fog)">
    <strong style="display:block; margin-bottom:4px">What lives here</strong>
    The reading frame (§0–1) followed by the four architecture diagrams, rendered from
    <code>docs/architecture/*.mmd</code> on every diagram-regen — a
    source-derived reference that doesn't drift. The decision record
    lives in the repo as <code>docs/architecture-decisions.md</code>,
    one consolidated current-truth document. Hosts, software-stack
    tables, and provider lists used to live here too as inline
    literals — they consistently drifted and were removed.
    Tenant-specific operator infrastructure (host registry, SaaS
    integrations) is per-deployment data, not core BOSS reference.
  </div>

  <nav
    aria-label="Knowledge Base jump nav"
    style="display:flex; flex-wrap:wrap; gap:8px; padding:12px 16px; margin-bottom:8px; background:var(--ink-raised); border:1px solid var(--hairline); border-radius:8px; font-size:13px"
  >
    <span style="color:var(--static); margin-right:4px">Jump to:</span>
    <a href="#it-layers"      style="color:var(--fog)">0 · The three layers</a>
    <span style="color:var(--static)">·</span>
    <a href="#it-framing"     style="color:var(--fog)">1 · Execution lens</a>
    <span style="color:var(--static)">·</span>
    <a href="#it-primitives"  style="color:var(--fog)">2 · Primitives</a>
    <span style="color:var(--static)">·</span>
    <a href="#it-service-map" style="color:var(--fog)">3 · Service map</a>
    <span style="color:var(--static)">·</span>
    <a href="#it-deployment"  style="color:var(--fog)">4 · Deployment</a>
    <span style="color:var(--static)">·</span>
    <!-- Each label promises what the catalog calls the page it lands on
         (200d474c). /it/design is where the design-doc packets — the
         decision record in the making — are read (d133ebf0); no repo
         file is linked, because on any other deployer's instance it
         would point at our LAN-only forge. -->
    <a href={href('/it/registry')} style="color:var(--fog)">Registry ↗</a>
    <span style="color:var(--static)">·</span>
    <a href={href('/it/design')} style="color:var(--fog)">Design decisions ↗</a>
  </nav>

  <div class="tab-content" style="display:flex; flex-direction:column; gap:24px; padding:16px 0">

    <section id="it-layers" class="tab-section tab-section-wide" style="scroll-margin-top:16px">
      <h3 style="margin-top:0">0. The reading frame — three layers</h3>
      <p class="prose" style="margin-bottom:16px">
        <strong>The network is the substrate. The fat protocols dictate the current
        operating model. The actors run it.</strong> Three layers, each replaceable
        without disturbing the others — which is what lets a company change how it
        works without rebuilding what it works <em>on</em>. Read every new workflow,
        page, or abstraction against this frame first. Canonical statement:
        <code>docs/design/the-three-layers.md</code>.
      </p>

      <div class="layers">
        <div class="layer layer-actors">
          <span class="layer-n">3</span>
          <div>
            <strong>The actors run it</strong>
            <p>
              Humans and registered agents are the CPUs — nothing moves without an
              actor claiming a step and doing it. Capability is enforced at the claim;
              bandwidth is finite, which is why stations hold rather than drop. Actors
              are not users of the system; they are the part of it that executes.
            </p>
          </div>
        </div>
        <div class="layer layer-protocols">
          <span class="layer-n">2</span>
          <div>
            <strong>The fat protocols dictate the current operating model</strong>
            <p>
              Workflows are protocols, and the meaning lives in the protocol row, not
              in the endpoints: the steps, the predicates that order them, the evidence
              each requires, the terminals, the obligations. That is why protocols are
              <em>registry data</em> — versioned, append-only, in-flight packets pinned
              to the version they were admitted under. <em>Current</em> is
              load-bearing: a protocol that cannot be replaced without a deploy has
              leaked into the substrate, and that leak is the defect to hunt.
            </p>
          </div>
        </div>
        <div class="layer layer-network">
          <span class="layer-n">1</span>
          <div>
            <strong>The network is the substrate</strong>
            <p>
              Packets (a Job is an immutable envelope plus a protocol set fixed at
              admission), stations (data-defined priority queues that route or hold),
              routes, the log, and the one admission edge. This layer is physics — it
              has no opinion about what the work means.
            </p>
          </div>
        </div>
      </div>
    </section>

    <section id="it-framing" class="tab-section tab-section-wide" style="scroll-margin-top:16px">
      <h3 style="margin-top:0">1. The execution lens — State · Surfaces · Work</h3>
      <p class="prose" style="margin-bottom:16px">
        This is the lens <em>over</em> the three layers, not the foundation: it answers
        "how does one packet get executed". <strong>State</strong> is the machine's
        memory (event log + projections). <strong>Surfaces</strong> are how CPUs —
        humans and agents — observe memory well enough to pick their next action.
        <strong>Work</strong> is the typed transitions those CPUs fire: Steps flipping
        to <code>done</code>, governed by policy, recorded as immutable events.
      </p>
      <p class="prose" style="margin-bottom:16px">
        Its invariants still hold — human and agent executors are CPUs in the same
        machine, and new work types are registry rows rather than bespoke core code
        paths. But when this reading and the network framing above disagree,
        <strong>the network framing wins</strong>, because it is the one that survives
        changing the operating model.
      </p>
      {@render diagram(stateSurfacesWorkSvg, 'State / Surfaces / Work framing')}
    </section>

    <section id="it-primitives" class="tab-section tab-section-wide" style="scroll-margin-top:16px">
      <h3 style="margin-top:0">2. Primitives &amp; cross-cutting abstractions</h3>
      <p class="prose" style="margin-bottom:16px">
        Four foundational primitives — <strong>Subject</strong> (the identity-bearing
        thing work is about), <strong>Job</strong> (a bounded unit of coordinated
        work), <strong>Step</strong> (a typed transition inside a Job), and
        <strong>Event</strong> (the immutable record of a state change, and the system
        of record) — cover every business entity worth modeling.
      </p>
      <p class="prose" style="margin-bottom:16px">
        Three <em>supporting</em> concepts hang off those four — load-bearing
        infrastructure, not foundational vocabulary. The <strong>Class registry</strong>
        is the reference data each Subject kind owns: one table carries every taxonomy
        (roles, account types, asset models), so a tenant extends one without forking
        core. <strong>StepPlugins</strong> are UX extensions on Steps: a small JS bundle,
        shipped as a registry row, that renders a custom surface for a step kind.
        <strong>Policy</strong> is the privilege model: every write passes through
        <code>boss-policy</code>, and its rules are rows.
      </p>
      {@render diagram(primitivesSvg, 'Primitives and abstractions')}
    </section>

    <section id="it-service-map" class="tab-section tab-section-wide" style="scroll-margin-top:16px">
      <h3 style="margin-top:0">3. Service map (domains)</h3>
      <p class="prose" style="margin-bottom:16px">
        Every service in the port registry (<code>boss-ports</code>), grouped by
        crate tier — core state-machine OS, company-modeling modules, cross-tier
        orchestrators, and the one example tenant. Services talk
        only through typed cross-service client crates; no direct DB access between
        services.
      </p>
      {@render diagram(serviceMapSvg, 'Service map')}
    </section>

    <section id="it-deployment" class="tab-section tab-section-wide" style="scroll-margin-top:16px">
      <h3 style="margin-top:0">4. Deployment topology</h3>
      <p class="prose" style="margin-bottom:16px">
        What a deployer runs: the open-source quickstart,
        <code>infra/oss-quickstart/</code>. Docker compose brings up Postgres, NATS
        and one <code>boss-services</code> container, whose launcher starts every
        service in the port registry that the tenant's manifest asks for, the
        gateway last. A one-shot <code>boss-init</code> converges the schema and
        the platform Workflow bundle on every start. The gateway serves the SPA
        and every <code>/api/*</code> route on port 4443 over plain HTTP; TLS is
        the job of a reverse proxy you put in front of it.
      </p>
      <p class="prose" style="margin-bottom:16px">
        <!-- Where THIS instance runs is data the estate registry holds and
             /it/estate renders; drawing it here would be a second copy
             that drifts (34717528). -->
        How this instance runs is data, not a drawing:
        <a href={href('/it/estate')}>This instance's estate, live ↗</a>
      </p>
      {@render diagram(deploymentSvg, 'Deployment topology')}
    </section>

  </div>
</div>
