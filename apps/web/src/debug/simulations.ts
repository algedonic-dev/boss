// Debug actions — operator-facing dogfood flows + sim-control
// shortcuts. Each action runs from the floating gear in the lower-
// right; the log panel shows what happened. Brewery-tenant focused;
// used-device-shop sims (refurb, service ticket) lived here in v1
// but were removed 2026-05-01 since they don't apply to the OSS
// playground tenant.
//
// Job-creating flows reuse already-seeded subjects (no net-new
// accounts or systems — we read whatever's in the DB), POST through
// the real API surface, and tag rows `debug-sim` so they're easy to
// find + prune later. Sim-control actions hit the new
// `/api/jobs/sim-clock/{pause,resume}` endpoints.

import { appNow, appToday } from '@boss/web-kit/sim-clock';

export type SimLogger = (msg: string) => void;

type StepTemplate = {
  kind: string;
  title: string;
  metadata: Record<string, unknown>;
  sign_offs_required?: string[];
};

const DEBUG_OWNER = 'emp-001';
const DEBUG_TAG = 'debug-sim';

function today(): string {
  return appToday();
}

async function fetchJson(url: string, init?: RequestInit): Promise<unknown> {
  const r = await fetch(url, {
    credentials: 'same-origin',
    ...init,
    headers: {
      'Content-Type': 'application/json',
      accept: 'application/json',
      ...(init?.headers ?? {}),
    },
  });
  if (!r.ok) {
    const text = await r.text().catch(() => '');
    throw new Error(`${r.status} ${r.statusText}${text ? `: ${text.slice(0, 200)}` : ''}`);
  }
  const ct = r.headers.get('content-type') ?? '';
  if (ct.includes('application/json')) return r.json();
  return null;
}

async function createJob(params: {
  kind: string;
  subject: Record<string, unknown>;
  title: string;
}): Promise<string> {
  const jobId = crypto.randomUUID();
  await fetchJson('/api/jobs', {
    method: 'POST',
    body: JSON.stringify({
      id: jobId,
      kind: params.kind,
      subject: params.subject,
      title: params.title,
      owner_id: DEBUG_OWNER,
      status: 'open',
      priority: 'standard',
      opened_on: today(),
      due_on: null,
      closed_on: null,
      metadata: {},
      tags: [DEBUG_TAG],
    }),
  });
  return jobId;
}

async function closeStep(
  jobId: string,
  step: StepTemplate,
  sortOrder: number,
): Promise<void> {
  const stepId = crypto.randomUUID();
  await fetchJson(`/api/jobs/${encodeURIComponent(jobId)}/steps`, {
    method: 'POST',
    body: JSON.stringify({
      id: stepId,
      job_id: jobId,
      kind: step.kind,
      title: step.title,
      status: 'completed',
      assignee_id: DEBUG_OWNER,
      sort_order: sortOrder,
      blocked_by: [],

      completed_on: today(),
      metadata: step.metadata,
      notes: null,
    }),
  });
}

async function runSteps(
  log: SimLogger,
  jobId: string,
  steps: ReadonlyArray<StepTemplate>,
): Promise<void> {
  for (let i = 0; i < steps.length; i++) {
    const s = steps[i]!;
    log(`  [${i + 1}/${steps.length}] ${s.kind} — ${s.title}`);
    await closeStep(jobId, s, i);
  }
}

// ---------------------------------------------------------------------------
// Flows
// ---------------------------------------------------------------------------

export async function placeShopOrder(log: SimLogger): Promise<void> {
  // Programmatic equivalent of the /shop checkout flow — opens a
  // direct-shop-order Job for one half-barrel keg of Pale Ale,
  // overlays line_items onto the shipment + billing steps so the
  // shipping.create + inventory.parts.consume side effects fire
  // when an operator walks the steps to done. End-to-end: the
  // existing /shop flow runs the same path.
  log('Opening direct-shop-order…');
  const sku = 'FP-PALE-1-2-BBL';
  const qty = 1;
  const unitPriceCents = 13500;
  const todayIso = today();

  const jobId = await createJob({
    kind: 'direct-shop-order',
    subject: { subject_kind: 'account', id: 'acc-direct-shop' },
    title: `[sim] /shop — ${qty}× ${sku}`,
  });
  log(`Job ${jobId.slice(0, 8)}… created`);

  // Resolve the materialized step ids so we can overlay line_items.
  const stepsResp = (await fetchJson(`/api/jobs/${jobId}/steps`)) as Array<{
    id: string;
    kind: string;
    metadata?: Record<string, unknown>;
  }>;
  const shipment = stepsResp.find((s) => s.kind === 'shipment');
  const billing = stepsResp.find((s) => s.kind === 'billing');
  const lineItems = [{ part_sku: sku, qty, unit_price_cents: unitPriceCents }];
  const consumesProducts = [{ sku, qty, location_id: 'loc-brewery-brewhouse' }];

  if (shipment) {
    await fetchJson(`/api/jobs/${jobId}/steps/${shipment.id}`, {
      method: 'PUT',
      body: JSON.stringify({
        metadata: {
          ...(shipment.metadata ?? {}),
          line_items: lineItems,
          consumes_products: consumesProducts,
        },
      }),
    });
    log(`  shipment step overlaid (${shipment.id.slice(0, 8)}…)`);
  }
  if (billing) {
    await fetchJson(`/api/jobs/${jobId}/steps/${billing.id}`, {
      method: 'PUT',
      body: JSON.stringify({
        metadata: {
          ...(billing.metadata ?? {}),
          line_items: lineItems,
          amount_cents: qty * unitPriceCents,
        },
      }),
    });
    log(`  billing step overlaid (${billing.id.slice(0, 8)}…)`);
  }
  log(`Order opened — walk the steps to done at /jobs/${jobId} (today=${todayIso})`);
}

type SimClockState = {
  current_sim_date: string;
  epoch_start_date: string | null;
  epoch_end_date: string | null;
  paused: boolean;
};

async function readSimClock(): Promise<SimClockState | null> {
  const live = (await fetchJson('/api/jobs/live')) as { sim_clock?: SimClockState | null };
  return live.sim_clock ?? null;
}

export async function showSimClock(log: SimLogger): Promise<void> {
  const c = await readSimClock();
  if (!c) {
    log('No sim_clock row (in-memory adapter or fresh DB).');
    return;
  }
  log(`current = ${c.current_sim_date}`);
  log(`epoch   = ${c.epoch_start_date ?? '?'} → ${c.epoch_end_date ?? '?'}`);
  log(`status  = ${c.paused ? 'PAUSED' : 'running'}`);
}

export async function toggleSimPause(log: SimLogger): Promise<void> {
  const c = await readSimClock();
  if (!c) {
    log('No sim_clock row to toggle.');
    return;
  }
  const path = c.paused ? '/api/jobs/sim-clock/resume' : '/api/jobs/sim-clock/pause';
  log(c.paused ? 'Resuming sim…' : 'Pausing sim…');
  const after = (await fetchJson(path, { method: 'POST' })) as SimClockState | null;
  if (!after) {
    log('No state returned (adapter mismatch).');
    return;
  }
  log(`current = ${after.current_sim_date}`);
  log(`status  = ${after.paused ? 'PAUSED' : 'running'}`);
}

export async function showResetInstructions(log: SimLogger): Promise<void> {
  // Reset-to-baseline runs as a sudo shell script (drops + recreates
  // the DB, replays the canonical seed, restarts services). Until we
  // ship a server-side wrapper with elevated privileges this stays a
  // host-side operator action; surface the command verbatim so the
  // operator can copy + paste from a terminal session.
  log('Reset-to-baseline (operator action — run on the host):');
  log('  sudo /opt/boss/infra/postgres/reset-to-baseline.sh');
  log('Stops the sim daemon, drops + recreates the live DB,');
  log('replays the canonical seed bundle, resets sim_clock,');
  log('and restarts the daemon at epoch_start_date. ~5 min.');
}

export async function placeWholesaleOrder(log: SimLogger): Promise<void> {
  // Wholesale-keg-order — B2B path, opens a Job for an existing
  // brewery account placing a multi-keg order. Distinct from the
  // direct-shop-order flow (which is the consumer /shop path):
  // wholesale ships full pallets, bills net-30 instead of card.
  log('Opening wholesale-keg-order…');
  const sku = 'FP-PALE-1-2-BBL';
  const qty = 12;
  const unitPriceCents = 13500;
  const todayIso = today();

  const jobId = await createJob({
    kind: 'wholesale-keg-order',
    subject: { subject_kind: 'account', id: 'acc-direct-shop' },
    title: `[sim] Wholesale — ${qty}× ${sku}`,
  });
  log(`Job ${jobId.slice(0, 8)}… created`);

  const stepsResp = (await fetchJson(`/api/jobs/${jobId}/steps`)) as Array<{
    id: string;
    kind: string;
    metadata?: Record<string, unknown>;
  }>;
  const shipment = stepsResp.find((s) => s.kind === 'shipment');
  const billing = stepsResp.find((s) => s.kind === 'billing');
  const lineItems = [{ part_sku: sku, qty, unit_price_cents: unitPriceCents }];
  const consumesProducts = [{ sku, qty, location_id: 'loc-brewery-brewhouse' }];

  if (shipment) {
    await fetchJson(`/api/jobs/${jobId}/steps/${shipment.id}`, {
      method: 'PUT',
      body: JSON.stringify({
        metadata: {
          ...(shipment.metadata ?? {}),
          line_items: lineItems,
          consumes_products: consumesProducts,
        },
      }),
    });
    log(`  shipment step overlaid (${shipment.id.slice(0, 8)}…)`);
  }
  if (billing) {
    await fetchJson(`/api/jobs/${jobId}/steps/${billing.id}`, {
      method: 'PUT',
      body: JSON.stringify({
        metadata: {
          ...(billing.metadata ?? {}),
          line_items: lineItems,
          amount_cents: qty * unitPriceCents,
        },
      }),
    });
    log(`  billing step overlaid (${billing.id.slice(0, 8)}…)`);
  }
  log(`Wholesale opened — walk steps to done at /jobs/${jobId} (today=${todayIso})`);
}

export async function triggerAnomaly(log: SimLogger): Promise<void> {
  // Open a vendor-delay incident — the most common brewery anomaly
  // (malt-supplier delay, hop-supplier shortage). Surfaces as an
  // open Job that the sim's daily generators don't auto-create
  // (those bias toward routine flow), so the operator can practice
  // working it through to resolution.
  const refId = `anomaly-${Date.now().toString(36)}`;
  log(`Opening vendor-delay incident (ref ${refId})…`);

  const jobId = await createJob({
    kind: 'vendor-incident',
    subject: { subject_kind: 'custom', custom_kind: 'incident', ref_id: refId },
    title: '[sim] Malt supplier — 2-week delivery delay',
  });
  log(`Incident Job ${jobId.slice(0, 8)}… created`);
  log('Walk steps to acknowledge → mitigate → resolve at the Job page.');
}

export async function runHire(log: SimLogger): Promise<void> {
  const refId = `sim-${Date.now().toString(36)}`;
  log(`Opening hiring Job (ref ${refId})…`);

  const jobId = await createJob({
    kind: 'hiring',
    subject: { subject_kind: 'custom', custom_kind: 'hiring', ref_id: refId },
    title: '[sim] Hire a cellar tech',
  });
  log(`Hiring Job ${jobId.slice(0, 8)}… created`);

  // Sim-time + 7 days — the demo runs in sim-time, so a wallclock
  // interview date would land years off the brewery's calendar.
  const interviewAt = new Date(appNow().getTime() + 7 * 24 * 3600 * 1000).toISOString();

  await runSteps(log, jobId, [
    {
      kind: 'generic',
      title: 'Open requisition',
      metadata: { department: 'cellar', seniority: 'mid' },
    },
    {
      kind: 'generic',
      title: 'Screen applicants',
      metadata: { screened: 12, shortlist: 4 },
    },
    {
      kind: 'scheduling',
      title: 'Schedule interview loop',
      metadata: { location: 'HQ', scheduled_at: interviewAt, duration_minutes: 240 },
    },
    {
      kind: 'approval',
      title: 'Offer approval',
      metadata: { decision: 'approved' },
    },
    {
      kind: 'checklist',
      title: 'Onboarding checklist',
      metadata: {
        items: [
          { label: 'offer accepted', checked: true },
          { label: 'background check', checked: true },
          { label: 'equipment issued', checked: true },
          { label: 'day-one orientation scheduled', checked: true },
        ],
      },
    },
  ]);

  log('Hire closed ✓');
}
