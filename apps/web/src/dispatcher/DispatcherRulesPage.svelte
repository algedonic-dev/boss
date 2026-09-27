<script lang="ts">
  // /it/registry/rules — list the ACTIVE dispatcher rules, each linking
  // to its editor. Authoring sibling of the /it/registry/dispatcher cascade viz.
  // Models the step-plugins list page (PageHeader/Section + data-table +
  // load/error state). Writes flow through ./ruleAuthoring.

  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import Section from '@boss/web-kit/ui/Section.svelte';
  import Link from '@boss/web-kit/ui/Link.svelte';
  import Breadcrumb from '@boss/web-kit/ui/Breadcrumb.svelte';
  import { listActiveRules, type DispatcherRule } from './ruleAuthoring';
  import type { AuthoredRegistry } from './types';
  import { describeTrigger } from './cascadeToGraph';
  import { ruleHandlers, ruleProvenance } from './ruleProvenance';
  import { fetchRuleFirings, ruleActivity, type RuleFirings } from './ruleFirings';
  import type { Remote } from '../data/remote';
  import { href } from '../router';

  let rules = $state<ReadonlyArray<DispatcherRule>>([]);
  // Where the whys were read from (backlog f9e34a2c): an unset or
  // unreadable BOSS_DISPATCHER_RULES serves `why: null, authored: false`
  // on every row, and only this block says the rows cannot be judged.
  let registry = $state<AuthoredRegistry | null>(null);
  let loading = $state(true);
  let error = $state<string | null>(null);
  // When each rule last fired and whether it is dead-lettering
  // (backlog 43c4451a) — a second read, beside the list rather than in
  // front of it: the rules still paint when this one fails, and each
  // activity cell then says "unknown" rather than "none".
  let firings = $state<Remote<RuleFirings>>({ kind: 'loading' });

  async function load(): Promise<void> {
    loading = true;
    try {
      const read = await listActiveRules();
      rules = read.rules;
      registry = read.authoredRegistry;
      error = null;
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      loading = false;
    }
  }

  $effect(() => {
    void load();
    void fetchRuleFirings().then((r) => {
      firings = r;
    });
  });

  let sorted = $derived([...rules].sort((a, b) => a.name.localeCompare(b.name)));
</script>

<div class="catalog theme-exec">
  <Breadcrumb to={href('/it/registry/dispatcher')}>← Dispatcher cascade</Breadcrumb>
  <PageHeader
    eyebrow="Platform · Dispatcher rules"
    title="Dispatcher rules"
    subtitle={loading
      ? 'Loading…'
      : error
        ? // A failed read leaves `rules` at [] — counting that painted the
          // empty registry's "0 active rules" above the failure line
          // (backlog 14371116). The count is unknown, so say so.
          'Rule count unknown — the registry read failed'
        : `${rules.length} active rule${rules.length === 1 ? '' : 's'} — the side-effect wiring boss-dispatcher runs`}
  />

  <!-- "+ New rule" opens the authoring guidance, not a form (backlog
       7d9df2fe, design ff1c3615): a product rule made in the SPA was
       retired by the dispatcher's boot seed at the next restart. -->
  <div style="padding:0 24px 16px; display:flex; gap:12px; align-items:center">
    <Link to={href('/it/registry/rules/new')} className="btn btn-primary">
      + New rule
    </Link>
    <span style="font-size:13px">
      A rule lasts when it is written down: a file under infra/dispatcher/rules/, or a
      tenant's seeds/rules.toml.
    </span>
  </div>

  {#if error}
    <!-- load-failed + role=alert: the shared failure marker the outage
         crawl asserts (tests/mocked/_routes.ts FAILURE_MARKER; backlog
         cae1a377). Without it the route sat in the crawl's SILENT map. -->
    <p class="empty load-failed" role="alert" style="margin:0 24px">Failed to load: {error}</p>
  {/if}

  {#if !error && registry?.error}
    <!-- The rules read, the whys did not (backlog f9e34a2c): every row
         then says unknown for its file, never "live only". -->
    <p class="registry-unread" role="status" style="margin:0 24px 12px; color:var(--warn)">
      The authored registry could not be read ({registry.dir ?? 'no directory set'}):
      {registry.error} — no rule's why or file can be shown.
    </p>
  {/if}

  {#if rules.length === 0 && !loading && !error}
    <p class="empty" style="padding:0 24px">
      No active dispatcher rules. A rule is authored as a file or a tenant seed —
      <Link to={href('/it/registry/rules/new')}>+ New rule</Link> says how.
    </p>
  {/if}

  {#if rules.length > 0}
    <div class="tab-grid">
      <Section title="Active rules" wide>
        <table class="data-table data-table-striped">
          <thead>
            <tr>
              <th>Rule</th>
              <th>Trigger</th>
              <th>Handler</th>
              <th class="num">Version</th>
              <th>Declared by</th>
              <th>Last fired</th>
              <th>Dead-letters</th>
            </tr>
          </thead>
          <tbody>
            {#each sorted as r (r.name)}
              {@const act = ruleActivity(r, firings)}
              {@const prov = ruleProvenance(r, registry)}
              <tr>
                <td>
                  <Link to={href(`/it/registry/rules/${encodeURIComponent(r.name)}`)}>
                    <span class="mono">{r.name}</span>
                  </Link>
                  <!-- The rule file's why (backlog f9e34a2c) — folded, as
                       a why runs to paragraphs. -->
                  {#if r.why}
                    <details class="why">
                      <summary>why</summary>
                      <p style="white-space:pre-line; margin:4px 0 0; font-size:13px">{r.why.trim()}</p>
                    </details>
                  {/if}
                </td>
                <!-- A scheduled rule has no on_event (on_event XOR
                     schedule); this cell used to render it blank
                     (backlog ee86a789). The when guard sits under it:
                     two rules on one topic differ only there. -->
                <td>
                  <code class="mono" style="font-size:12px">{describeTrigger(r)}</code>
                  {#if r.when}
                    <div class="when" style="font-size:12px; color:var(--text-dim)">
                      when <code class="mono">{r.when}</code>
                    </div>
                  {/if}
                </td>
                <!-- The handler, not the count of do steps: 65 of 65 live
                     rows had exactly one (backlog f9e34a2c). -->
                <td><code class="mono" style="font-size:12px">{ruleHandlers(r)}</code></td>
                <td class="num">{r.version}</td>
                <!-- A live-only product rule is enforced now and retired
                     at the next boot — the §9a drift `authored` exposes. -->
                <td class="provenance" class:drift={prov.kind === 'drift'} title={prov.why}
                    style={prov.kind === 'drift' ? 'color:var(--err)' : undefined}>
                  {prov.label}
                </td>
                <td title={act.lastFiredWhy}>{act.lastFired}</td>
                <!-- A dead-letter newer than the newest firing is a rule
                     failing NOW — the stalled shape an idle row used to
                     share (backlog 43c4451a). The count links to the
                     packet holding the newest, where the error is. -->
                <td class="dead-letters" class:failing={act.failing} title={act.deadLettersWhy}
                    style={act.failing ? 'color:var(--err)' : undefined}>
                  {#if act.deadLetterJob}
                    <Link to={href(`/jobs/${act.deadLetterJob}`)}>{act.deadLetters}</Link>
                  {:else}
                    {act.deadLetters}
                  {/if}
                </td>
              </tr>
            {/each}
          </tbody>
        </table>
      </Section>
    </div>
  {/if}
</div>
