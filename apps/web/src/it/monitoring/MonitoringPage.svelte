<script lang="ts">
  // IT Monitoring — live state of every Boss service. Service map
  // + health probes, infrastructure pointers, churn watchlist, ML
  // model oversight. Pairs with /it/kb (the static reference);
  // together they make up the IT department surface.
  //
  // Migrated from /cto on 2026-05-03 as part of the IT-as-department
  // IA shift. The CTO dashboard framing was wrong — IT is a
  // department, the CTO is a role; the surface follows the
  // department, not the role.

  import PageHeader from '@boss/web-kit/ui/PageHeader.svelte';
  import Section from '@boss/web-kit/ui/Section.svelte';
  import Link from '@boss/web-kit/ui/Link.svelte';
  import EntityLink from '@boss/web-kit/ui/EntityLink.svelte';
  import MlModelsPanel from './MlModelsPanel.svelte';
  import { href } from '../../router';
  import { moduleEnabled } from '@boss/web-kit/session/manifest.svelte';
  import { PORTS } from '../../_generated/ports';

  type ServiceStatus = 'healthy' | 'degraded' | 'down' | 'unknown';
  type ServiceInfo = {
    name: string;
    port: number;
    description: string;
    /// /api/<slug>/health route prefix. `null` means the service
    /// has no SPA-proxied health endpoint (NATS, Postgres, the
    /// gateway itself) — those render as "n/a" instead of probing.
    healthSlug: string | null;
  };

  // Per-service description. Keyed by the canonical name from
  // `boss-ports::lib::PAIRED + SOLO`. New services land in
  // `_generated/ports.ts` automatically — they show up here as
  // "(no description)" until someone fills one in. That's the
  // intended pressure: stay current with descriptions, but never
  // miss a service silently.
  const DESCRIPTIONS: Record<string, string> = {
    jobs: 'Jobs + Steps + Scheduling, JobKind + StepPlugin registries',
    people: 'Employees, contacts, HR, org chart, certifications, account team membership',
    assets: 'Tracked physical units + event log, device-insights projection',
    catalog: 'System models, parts, consumables, failure modes',
    commerce: 'Opportunities, invoices, agreements',
    inventory: 'Stock, POs, vendor CRM, warehouse-status projection',
    shipping: 'Inbound + outbound shipments + tracking scans',
    messages: 'Unified inbox: DMs + system signals',
    calendar: 'Reservations across employees / systems / accounts',
    policy: 'Row-level authorization',
    ledger: 'GL projection, posting rules, period locks',
    content: 'Bulletins + manual sections + file references',
    ml: 'Inference platform — declarative-rule + heuristic-formula plugins',
    docs: 'Design-doc reader + decision tracker (serves /api/design)',
    classes: 'Class registry — tenant-extensible taxonomies',
    locations: 'Locations registry',
    'subject-kinds': 'Custom Subject kinds registry',
    products: 'Finished-product catalog + per-location on-hand inventory',
    observability: 'NATS → SSE fan-out, cybernetics rollup snapshot',
  };

  // Derive the row list from the canonical PORTS registry. Adds
  // the non-service infrastructure rows (gateway + NATS + Postgres)
  // by hand since they aren't BOSS HTTP services. Health probes use
  // the service name as the slug; the gateway exposes
  // `/api/<name>/health` for every PORTS entry.
  const SERVICES: ReadonlyArray<ServiceInfo> = [
    { name: 'boss-gateway', port: 4443, healthSlug: null, description: 'TLS termination, reverse proxy, static SPA serving' },
    ...PORTS.map((p) => ({
      name: `boss-${p.name}-api`,
      port: p.prod,
      healthSlug: p.name,
      description: DESCRIPTIONS[p.name] ?? '(no description)',
    })),
    { name: 'NATS', port: 4222, healthSlug: null, description: 'Event bus — every service fans out via DomainPublisher' },
    { name: 'PostgreSQL', port: 5432, healthSlug: null, description: 'Primary data store, audit_log hash-chained' },
  ];

  // Live status, polled on an interval. Defaults to `unknown`; each
  // probe resolves to healthy / degraded / down / unknown and the row
  // updates as responses land. Re-polling (not a one-shot on mount) is
  // the fix for services that were cold or slow at first paint sticking
  // red forever — a transient now recovers to green on the next tick.
  let serviceStatus = $state<Record<string, ServiceStatus>>({});

  // Classify one /health response. Only a real JSON health body counts
  // as healthy/degraded. A 200 that isn't JSON is the SPA index served
  // by the gateway's static fall-through — i.e. there's no health route
  // for this slug (e.g. boss-dispatcher is a NATS consumer with no HTTP
  // API; some services aren't proxied at /api/<slug>/health) — that's
  // `unknown`, NOT a degraded service. A 4xx (no route / not probeable)
  // is likewise `unknown`. Only a 5xx or a network/timeout failure — a
  // genuinely unreachable upstream — is `down`. This stops live, healthy
  // deployments painting half their rows red/yellow.
  async function probeHealth(slug: string): Promise<ServiceStatus> {
    try {
      const r = await fetch(`/api/${slug}/health`, {
        // Health routes are auth-bypass at the gateway; keep a modest
        // timeout so a flap doesn't hang the row (the interval retries).
        signal: AbortSignal.timeout(5000),
      });
      if (r.status >= 500) return 'down';
      if (!r.ok) return 'unknown';
      const body = (await r.json().catch(() => null)) as { status?: string } | null;
      if (!body || typeof body.status !== 'string') return 'unknown';
      return body.status === 'ok' ? 'healthy' : 'degraded';
    } catch {
      return 'down';
    }
  }

  $effect(() => {
    let cancelled = false;
    const probeable = SERVICES.filter((s) => s.healthSlug !== null);
    const pollAll = () => {
      for (const s of probeable) {
        probeHealth(s.healthSlug as string).then((status) => {
          if (!cancelled) serviceStatus = { ...serviceStatus, [s.name]: status };
        });
      }
    };
    pollAll();
    const id = setInterval(pollAll, 10_000);
    return () => {
      cancelled = true;
      clearInterval(id);
    };
  });

  function statusFor(s: ServiceInfo): ServiceStatus {
    if (s.healthSlug === null) return 'unknown'; // n/a — rendered as a dash
    return serviceStatus[s.name] ?? 'unknown';
  }

  type AgentCost = {
    agent: string;
    cost: { input_tokens: number; output_tokens: number; usd_micros: number };
    window: string;
  };
  type VmCosts = { vm_id: string; body?: ReadonlyArray<AgentCost>; error?: string };

  let agentCosts = $state<VmCosts[]>([]);
  let counts = $state({ devices: 0, serviceJobs: 0, accounts: 0, models: 0, employees: 0 });

  // Risk-score panel data
  type RiskScore = {
    account_id: string;
    account_name: string;
    score: number;
    top_factor: string;
  };
  type RiskState =
    | { kind: 'loading' }
    | { kind: 'error'; message: string }
    | { kind: 'ready'; scores: ReadonlyArray<RiskScore> };
  let riskState: RiskState = $state<RiskState>({ kind: 'loading' });

  $effect(() => {
    let cancelled = false;
    (async () => {
      try {
        const r = await fetch('/api/snapshot');
        if (r.ok) {
          const data = (await r.json()) as { costs?: VmCosts[] };
          if (!cancelled && data.costs) agentCosts = data.costs;
        }
      } catch {
        // ignore
      }
      try {
        const [pResp, mResp, eResp, dResp, jResp] = await Promise.all([
          fetch('/api/people/accounts'),
          fetch('/api/catalog/models'),
          fetch('/api/people'),
          fetch('/api/assets/systems?limit=1'),
          fetch('/api/jobs/summary?kind=field-service&status=open'),
        ]);
        if (!cancelled) {
          if (pResp.ok) {
            const body = await pResp.json();
            counts.accounts = (Array.isArray(body) ? body : (body.data ?? [])).length;
          }
          if (mResp.ok) {
            const body = await mResp.json();
            counts.models = (Array.isArray(body) ? body : (body.data ?? [])).length;
          }
          if (eResp.ok) {
            const body = await eResp.json();
            counts.employees = (Array.isArray(body) ? body : []).length;
          }
          if (dResp.ok) {
            const body = await dResp.json();
            counts.devices = body.total ?? (body.data?.length ?? 0);
          }
          if (jResp.ok) {
            const body = (await jResp.json()) as { total: number };
            counts.serviceJobs = body.total ?? 0;
          }
        }
      } catch {
        // ignore
      }
      try {
        const r = await fetch('/api/people/accounts/risk-scores?limit=10');
        if (!r.ok) throw new Error(`${r.status}`);
        const body = (await r.json()) as { accounts: RiskScore[] };
        if (!cancelled) riskState = { kind: 'ready', scores: body.accounts };
      } catch (e) {
        if (!cancelled) riskState = { kind: 'error', message: String(e) };
      }
    })();
    return () => {
      cancelled = true;
    };
  });

  function statusColor(status: ServiceStatus): string {
    if (status === 'healthy') return '#16a34a';
    if (status === 'degraded') return '#eab308';
    if (status === 'unknown') return '#94a3b8';
    return '#dc2626';
  }
  function scoreTone(score: number): 'high' | 'mid' | 'low' {
    if (score >= 50) return 'high';
    if (score >= 25) return 'mid';
    return 'low';
  }

  // Agent cost aggregation
  let costRows = $derived.by(() => {
    const byAgent = new Map<string, { tokens: number; usd: number }>();
    for (const vm of agentCosts) {
      if (!vm.body) continue;
      for (const entry of vm.body) {
        const prev = byAgent.get(entry.agent) ?? { tokens: 0, usd: 0 };
        byAgent.set(entry.agent, {
          tokens: prev.tokens + entry.cost.input_tokens + entry.cost.output_tokens,
          usd: prev.usd + entry.cost.usd_micros / 1_000_000,
        });
      }
    }
    return [...byAgent.entries()].sort((a, b) => b[1].usd - a[1].usd);
  });
  let totalUsd = $derived(costRows.reduce((s, [, v]) => s + v.usd, 0));
</script>

<div class="catalog theme-exec">
  <PageHeader
    eyebrow="IT department · Monitoring"
    title="IT Monitoring"
    subtitle="Live state of every BOSS service — health probes, deployment topology, ML model oversight"
  />

  <div class="cto-grid">
    <Section title="Services" wide>
        <div class="cto-services">
          {#each SERVICES as s (s.name)}
            {@const status = statusFor(s)}
            {@const notProbed = s.healthSlug === null}
            <div
              class="cto-svc-card"
              title={notProbed
                ? `${s.name}: not probed from the SPA (external infrastructure — check uptime out-of-band)`
                : `${s.name}: ${status}`}
            >
              <div class="cto-svc-header">
                <span class="cto-dot" style={`background-color:${statusColor(status)}`}></span>
                <span class="cto-svc-name">{s.name}</span>
                <span class="cto-svc-port">:{s.port}</span>
              </div>
              <div class="cto-svc-desc">{s.description}</div>
              {#if notProbed}
                <div class="cto-svc-footnote">
                  Infra layer — not probed from the SPA.
                </div>
              {/if}
            </div>
          {/each}
        </div>
        <p class="cto-services-legend">
          BOSS services expose <code>/api/&lt;slug&gt;/health</code> and are probed
          live on page load. Gateway, NATS, and Postgres are infrastructure;
          their uptime is monitored out-of-band, not from this page.
        </p>
    </Section>

    <Section title="Data Pipeline">
        <dl class="kv cto-kv">
          <dt>Service requests</dt><dd class="num">{counts.serviceJobs}</dd>
          <dt>Tracked devices</dt><dd class="num">{counts.devices}</dd>
          <dt>Active accounts</dt><dd class="num">{counts.accounts}</dd>
          <dt>Catalog models</dt><dd class="num">{counts.models}</dd>
          <dt>Employees</dt><dd class="num">{counts.employees}</dd>
        </dl>
    </Section>

    <Section title="Churn watchlist">
        {#if riskState.kind === 'loading'}
          <p class="empty">Scoring accounts…</p>
        {:else if riskState.kind === 'error'}
          <p class="empty">Could not load watchlist ({riskState.message}).</p>
        {:else if riskState.scores.length === 0}
          <p class="empty">No at-risk accounts.</p>
        {:else}
          <table class="data-table data-table-striped risk-table">
            <thead>
              <tr><th>Account</th><th class="num">Score</th><th>Why</th></tr>
            </thead>
            <tbody>
              {#each riskState.scores as s (s.account_id)}
                <tr>
                  <td>
                    <EntityLink
                      kind="account"
                      id={s.account_id}
                      label={s.account_name}
                      mono={false}
                    />
                  </td>
                  <td class="num">
                    <span class="risk-chip risk-chip-{scoreTone(s.score)}">{s.score}</span>
                  </td>
                  <td>{s.top_factor}</td>
                </tr>
              {/each}
            </tbody>
          </table>
          <p style="margin-top:12px; text-align:right">
            <Link to={href('/watchlist')}>
              View full watchlist →
            </Link>
          </p>
        {/if}
    </Section>

    <MlModelsPanel />

    <Section title="Agent Spend">
        {#if costRows.length === 0}
          <div class="cto-svc-desc">No agent activity in this window</div>
        {:else}
          <dl class="kv cto-kv">
            <dt>Total (last hour)</dt>
            <dd class="num">${totalUsd.toFixed(2)}</dd>
          </dl>
          <div class="cto-services" style="margin-top:8px">
            {#each costRows as [agent, data] (agent)}
              <div class="cto-svc-card" style="padding:8px 12px">
                <div class="cto-svc-header">
                  <span class="cto-svc-name">{agent}</span>
                  <span class="cto-svc-port">${data.usd.toFixed(4)}</span>
                </div>
                <div class="cto-svc-desc">{data.tokens.toLocaleString()} tokens</div>
              </div>
            {/each}
          </div>
        {/if}
    </Section>

    <Section title="Quick links">
        <div class="cto-links">
          <Link to={href('/ops')} className="cto-link">
            Cybernetics Observability
          </Link>
          <Link to={href('/it/monitoring/perf')} className="cto-link">
            Gateway Latency
          </Link>
          <Link to={href('/it/monitoring/events')} className="cto-link">
            Audit Log Tail
          </Link>
          {#if moduleEnabled('equipment')}
            <!-- Equipment-shaped surfaces. Brewery sets
                 [modules].equipment = false; these links hide.
                 Used-device-shop's tenant manifest defaults
                 missing keys to true so all three show. -->
            <Link to={href('/assets')} className="cto-link">
              Devices Overview
            </Link>
            <Link to={href('/catalog')} className="cto-link">
              Device Catalog
            </Link>
            <Link to={href('/it/monitoring/atlas')} className="cto-link">
              System Atlas
            </Link>
          {/if}
          <Link to={href('/exec')} className="cto-link">
            Exec Dashboard
          </Link>
        </div>
    </Section>
  </div>
</div>

<style>
  .cto-svc-footnote {
    font-size: 11px;
    color: #78716c;
    margin-top: 4px;
    font-style: italic;
  }
  .cto-services-legend {
    font-size: 12px;
    color: #57534e;
    margin-top: 12px;
    line-height: 1.5;
  }
  .cto-services-legend code {
    font-size: 11px;
    background: #f5f5f4;
    padding: 1px 4px;
    border-radius: 3px;
  }
</style>
