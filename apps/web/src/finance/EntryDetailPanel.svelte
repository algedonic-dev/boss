<script lang="ts">
  // Compact, read-only view of a single journal entry + lines.
  // Port of apps/web/src/finance/EntryDetailPanel.tsx (EntryDetailPanel
  // variant — no reverse button, simple fact-source rendering).

  import { formatUsd, type LedgerEntryDetail } from './ledger';
  import { shortId } from '../data/ids';

  type Props = {
    entry: LedgerEntryDetail | null;
    loading?: boolean;
  };
  let { entry, loading = false }: Props = $props();
</script>

{#if loading && !entry}
  <p class="empty">Loading entry…</p>
{:else if !entry}
  <p class="empty">No journal entry for this row yet.</p>
{:else}
  {@const e = entry}
  <div class="tb-entry-detail">
    <h4>
      Entry {shortId(e.id)} · posted {e.posted_on}
      <span class="ruleset-badge">RuleSet v{e.rule_version}</span>
    </h4>
    <table class="tb-entry-lines">
      <thead>
        <tr>
          <th class="c">Code</th>
          <th>Account</th>
          <th class="r">Debit</th>
          <th class="r">Credit</th>
        </tr>
      </thead>
      <tbody>
        {#each e.lines as l, i (i)}
          <tr>
            <td class="c mono">{l.account_code}</td>
            <td>{l.account_name}</td>
            <td class="r mono">{l.debit_cents > 0 ? formatUsd(l.debit_cents) : ''}</td>
            <td class="r mono">{l.credit_cents > 0 ? formatUsd(l.credit_cents) : ''}</td>
          </tr>
        {/each}
      </tbody>
    </table>
    <details class="tb-fact-payload">
      <summary>
        Fact: <code>{e.fact_kind}</code>
        {#if e.fact_source_table && e.fact_source_id}
          · {e.fact_source_table}:{e.fact_source_id}
        {/if}
      </summary>
      <pre>{JSON.stringify(e.fact_payload, null, 2)}</pre>
    </details>
  </div>
{/if}
