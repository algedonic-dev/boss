<script lang="ts">
  // Support dashboard — port of apps/web/src/support/SupportPage.tsx.
  //
  // Field-service-only view over /api/jobs filtered to kind=field-service,
  // joined with accounts + systems for the tabs.

  import PageHeader from '../ui/PageHeader.svelte';
  import { appNow } from '../shell/sim-clock.svelte';
  import Link from '../ui/Link.svelte';
  import Section from '../ui/Section.svelte';
  import OverflowBanner from '../ui/OverflowBanner.svelte';
  import type { Job } from '../jobs/types';
  import { shortId } from '../data/ids';
  import { href } from '../router';
  import { entityHref } from '../ui/entity-href';
  import { fetchPaged, isCapped, type Paged } from '../data/paginated';

  type Account = {
    id: string;
    name: string;
    tier: 'platinum' | 'gold' | 'silver';
  };
  type Asset = {
    asset_id: string;
    account_id: string | null;
  };

  type Tab = 'overview' | 'active' | 'account-health';

  const TABS: ReadonlyArray<{ id: Tab; label: string }> = [
    { id: 'overview', label: 'Overview' },
    { id: 'active', label: 'Active Cases' },
    { id: 'account-health', label: 'Account Health' },
  ];

  let jobsPage = $state<Paged<Job> | null>(null);
  let accounts = $state<Account[]>([]);
  let devicesPage = $state<Paged<Asset> | null>(null);
  let loading = $state(true);
  let tab = $state<Tab>('overview');

  let jobs = $derived(jobsPage?.data ?? []);
  let devices = $derived(devicesPage?.data ?? []);

  $effect(() => {
    let cancelled = false;
    loading = true;
    (async () => {
      try {
        const [jPaged, pResp, dPaged] = await Promise.all([
          fetchPaged<Job>('/api/jobs?kind=field-service&limit=5000'),
          fetch('/api/people/accounts'),
          fetchPaged<Asset>('/api/assets?limit=1000'),
        ]);
        const pBody = pResp.ok ? await pResp.json() : [];
        if (!cancelled) {
          jobsPage = jPaged;
          accounts = Array.isArray(pBody) ? pBody : (pBody.data ?? []);
          devicesPage = dPaged;
          loading = false;
        }
      } catch {
        if (!cancelled) loading = false;
      }
    })();
    return () => {
      cancelled = true;
    };
  });

  function isOpen(j: Job): boolean {
    return j.status !== 'closed' && j.status !== 'cancelled';
  }

  let openJobs = $derived(jobs.filter(isOpen));

  let accountIdsWithOpen = $derived.by(() => {
    const s = new Set<string>();
    for (const j of openJobs) {
      if (j.subject.subject_kind === 'account' && j.subject.id) {
        s.add(j.subject.id);
      }
    }
    return s;
  });

  let escalatedCount = $derived.by(() => {
    const now = appNow().getTime();
    return openJobs.filter((j) => {
      const daysOpen = Math.floor(
        (now - new Date(j.opened_on).getTime()) / 86_400_000,
      );
      return daysOpen > 14;
    }).length;
  });

  let activeRows = $derived.by(() => {
    const now = appNow().getTime();
    return openJobs
      .map((j) => {
        const daysOpen = Math.floor(
          (now - new Date(j.opened_on).getTime()) / 86_400_000,
        );
        const assetId =
          j.subject.subject_kind === 'asset' ? (j.subject.id ?? '') : '';
        const accountId =
          j.subject.subject_kind === 'account' ? (j.subject.id ?? '') : '';
        return {
          job: j,
          daysOpen,
          account: accounts.find((c) => c.id === accountId),
          device: devices.find((d) => d.asset_id === assetId),
        };
      })
      .sort((a, b) => b.daysOpen - a.daysOpen);
  });

  let accountHealthRows = $derived.by(() => {
    type Row = {
      account: Account;
      openCount: number;
      deviceCount: number;
      lastDate: string | null;
    };
    const map = new Map<string, Row>();
    for (const c of accounts) {
      map.set(c.id, { account: c, openCount: 0, deviceCount: 0, lastDate: null });
    }
    for (const d of devices) {
      if (d.account_id) {
        const e = map.get(d.account_id);
        if (e) e.deviceCount++;
      }
    }
    for (const j of jobs) {
      if (j.subject.subject_kind !== 'account' || !j.subject.id) continue;
      const e = map.get(j.subject.id);
      if (!e) continue;
      if (isOpen(j)) e.openCount++;
      if (!e.lastDate || j.opened_on > e.lastDate) e.lastDate = j.opened_on;
    }
    return [...map.values()]
      .filter((e) => e.openCount > 0)
      .sort((a, b) => b.openCount - a.openCount);
  });
</script>

<div class="catalog theme-exec">
  <PageHeader
    eyebrow="Customer Support"
    title={isCapped(jobsPage)
      ? `${openJobs.length}+ open cases (window-only)`
      : `${openJobs.length} open cases`}
    subtitle="Field-service Jobs"
  />

  {#if isCapped(jobsPage)}
    <OverflowBanner
      showing={jobs.length}
      total={jobsPage!.total}
      noun="service jobs loaded"
      hint="Open-case + account-health counts on this page only consider this window; raise the cap or narrow by date."
    />
  {/if}
  {#if isCapped(devicesPage)}
    <OverflowBanner
      showing={devices.length}
      total={devicesPage!.total}
      noun="devices loaded for case-to-device joins"
      hint="Some active cases may show their device as '—' when its row is past the cap."
    />
  {/if}

  <nav class="tabs" role="tablist">
    {#each TABS as t (t.id)}
      <button
        type="button"
        role="tab"
        aria-selected={tab === t.id}
        class="tab {tab === t.id ? 'tab-active' : ''}"
        onclick={() => (tab = t.id)}
      >
        {t.label}
      </button>
    {/each}
  </nav>

  {#if loading}
    <p class="empty">Loading…</p>
  {:else if tab === 'overview'}
    <div class="tab-content" style="display:flex; flex-wrap:wrap; gap:16px; padding:16px 0">
      <Section title="Case volume">
          <dl class="kv">
            <dt>Total open service jobs</dt>
            <dd><strong>{openJobs.length}</strong></dd>
          </dl>
      </Section>
      <Section title="Coverage">
          <dl class="kv">
            <dt>Accounts with open jobs</dt>
            <dd><strong>{accountIdsWithOpen.size}</strong></dd>
            <dt>Escalated (&gt;14d open)</dt>
            <dd><strong style="color:#d97706">{escalatedCount}</strong></dd>
          </dl>
      </Section>
    </div>
  {:else if tab === 'active'}
    <section class="list-section" style="padding:16px 0">
      {#if activeRows.length === 0}
        <p class="empty">No open cases.</p>
      {:else}
        <table class="data-table data-table-striped">
          <thead>
            <tr>
              <th>Job</th>
              <th>Account</th>
              <th>Subject</th>
              <th class="num">Days open</th>
              <th>Status</th>
              <th>Title</th>
            </tr>
          </thead>
          <tbody>
            {#each activeRows as r (r.job.id)}
              <tr class="data-table-row-link">
                <td class="mono">
                  <Link to={entityHref('job', r.job.id)}>
                    {shortId(r.job.id)}
                  </Link>
                </td>
                <td>
                  {#if r.account}
                    {@const prac = r.account}
                    <Link to={entityHref('account', prac.id)}>
                      {prac.name}
                    </Link>
                  {:else}
                    —
                  {/if}
                </td>
                <td class="mono">
                  {#if r.device}
                    {@const dev = r.device}
                    <Link to={entityHref('asset', dev.asset_id)}>
                      {dev.asset_id}
                    </Link>
                  {:else}
                    —
                  {/if}
                </td>
                <td class="num">
                  {#if r.daysOpen > 14}
                    <strong style="color:#d97706">{r.daysOpen}d</strong>
                  {:else}
                    {r.daysOpen}d
                  {/if}
                </td>
                <td>{r.job.status.replace(/-/g, ' ')}</td>
                <td class="prose-cell">{r.job.title}</td>
              </tr>
            {/each}
          </tbody>
        </table>
      {/if}
    </section>
  {:else if tab === 'account-health'}
    <section class="list-section" style="padding:16px 0">
      {#if accountHealthRows.length === 0}
        <p class="empty">No account data.</p>
      {:else}
        <table class="data-table data-table-striped">
          <thead>
            <tr>
              <th>Account</th>
              <th>Tier</th>
              <th class="num">Open jobs</th>
              <th class="num">Equipment</th>
              <th>Last job</th>
            </tr>
          </thead>
          <tbody>
            {#each accountHealthRows as r (r.account.id)}
              <tr class="data-table-row-link">
                <td>
                  <Link to={entityHref('account', r.account.id)}>
                    {r.account.name}
                  </Link>
                </td>
                <td>{r.account.tier}</td>
                <td class="num"><strong>{r.openCount}</strong></td>
                <td class="num">{r.deviceCount}</td>
                <td>{r.lastDate ?? '—'}</td>
              </tr>
            {/each}
          </tbody>
        </table>
      {/if}
    </section>
  {/if}
</div>
