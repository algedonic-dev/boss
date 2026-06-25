// Monthly close package — one ZIP containing TB + IS + BS + CF for a
// given calendar month, plus a README with the date bounds and
// source-endpoint URLs for auditor traceability.
//
// Pure composition over the existing ledger loaders + csv helpers;
// no new backend surface. The month-picker input uses `<input
// type="month">` so callers just pass "YYYY-MM".

import {
  centsToDollars,
  dateStamp,
  rowsToCsv,
  type CsvColumn,
} from './csvExport';
import {
  loadBalanceSheet,
  loadCashFlow,
  loadIncomeStatement,
  loadTrialBalance,
  type BalanceSheet,
  type CashFlowStatement,
  type IncomeStatement,
  type StatementLine,
  type TrialBalanceResponse,
  type TrialBalanceRow,
} from './ledger';
import { buildZip, type ZipFile } from './zipStore';
import { appNow } from '@boss/web-kit/sim-clock';

export type MonthRange = Readonly<{
  /// "YYYY-MM" — the calendar month being closed.
  month: string;
  /// ISO date of the first day of the month (inclusive).
  from: string;
  /// ISO date of the last day of the month (inclusive).
  to: string;
}>;

/// Parse "YYYY-MM" into the from / to dates. Throws if the input
/// doesn't match the expected shape — callers get that from the
/// month-picker input, which only emits valid values.
export function monthRange(month: string): MonthRange {
  const m = /^(\d{4})-(\d{2})$/.exec(month);
  if (!m) throw new Error(`invalid month: ${month}`);
  const year = Number(m[1]);
  const mon = Number(m[2]);
  const from = `${m[1]}-${m[2]}-01`;
  // Last day of month: day 0 of the *next* month in local time, then
  // re-stringify. Using UTC would drift in non-UTC zones at month
  // boundaries; the backend handlers inclusive-compare on dates, not
  // timestamps, so the local day is the right unit.
  const lastDay = new Date(year, mon, 0).getDate();
  const to = `${m[1]}-${m[2]}-${String(lastDay).padStart(2, '0')}`;
  return { month, from, to };
}

type StatementRow = {
  section: string;
  account_code: string;
  account_name: string;
  amount_cents: number;
};

function flatten(
  section: string,
  lines: ReadonlyArray<StatementLine>,
): StatementRow[] {
  return lines.map((l) => ({
    section,
    account_code: l.account_code,
    account_name: l.account_name,
    amount_cents: l.amount_cents,
  }));
}

function incomeStatementCsv(d: IncomeStatement): string {
  const rows: StatementRow[] = [
    ...flatten('Revenue', d.revenue),
    { section: 'Revenue', account_code: '', account_name: 'Total revenue', amount_cents: d.total_revenue_cents },
    ...flatten('COGS', d.cogs),
    { section: 'COGS', account_code: '', account_name: 'Total COGS', amount_cents: d.total_cogs_cents },
    { section: 'Gross profit', account_code: '', account_name: 'Gross profit', amount_cents: d.gross_profit_cents },
    ...flatten('Operating expenses', d.operating_expenses),
    { section: 'Operating expenses', account_code: '', account_name: 'Total operating expenses', amount_cents: d.total_operating_expenses_cents },
    { section: 'Net income', account_code: '', account_name: 'Net income', amount_cents: d.net_income_cents },
  ];
  const columns: ReadonlyArray<CsvColumn<StatementRow>> = [
    { header: 'Section', value: (r) => r.section },
    { header: 'Account code', value: (r) => r.account_code },
    { header: 'Account name', value: (r) => r.account_name },
    { header: 'Amount', value: (r) => centsToDollars(r.amount_cents) },
    { header: 'Currency', value: () => d.currency },
  ];
  return rowsToCsv(rows, columns);
}

function balanceSheetCsv(d: BalanceSheet): string {
  const rows: StatementRow[] = [
    ...flatten('Assets', d.assets),
    { section: 'Assets', account_code: '', account_name: 'Total assets', amount_cents: d.total_assets_cents },
    ...flatten('Liabilities', d.liabilities),
    { section: 'Liabilities', account_code: '', account_name: 'Total liabilities', amount_cents: d.total_liabilities_cents },
    ...flatten('Equity', d.equity),
    { section: 'Equity', account_code: '', account_name: 'Total equity', amount_cents: d.total_equity_cents },
    { section: 'Total', account_code: '', account_name: 'Total liabilities + equity', amount_cents: d.total_liabilities_cents + d.total_equity_cents },
  ];
  const columns: ReadonlyArray<CsvColumn<StatementRow>> = [
    { header: 'Section', value: (r) => r.section },
    { header: 'Account code', value: (r) => r.account_code },
    { header: 'Account name', value: (r) => r.account_name },
    { header: 'Amount', value: (r) => centsToDollars(r.amount_cents) },
    { header: 'Currency', value: () => d.currency },
  ];
  return rowsToCsv(rows, columns);
}

function cashFlowCsv(d: CashFlowStatement): string {
  const rows: StatementRow[] = [
    { section: 'Operations', account_code: '', account_name: 'Net income', amount_cents: d.net_income_cents },
    ...flatten('Operations', d.working_capital_adjustments),
    ...flatten('Operations', d.non_cash_adjustments),
    { section: 'Operations', account_code: '', account_name: 'Cash from operations', amount_cents: d.cash_from_operations_cents },
    ...flatten('Investing', d.investing_activities),
    { section: 'Investing', account_code: '', account_name: 'Cash from investing', amount_cents: d.cash_from_investing_cents },
    ...flatten('Financing', d.financing_activities),
    { section: 'Financing', account_code: '', account_name: 'Cash from financing', amount_cents: d.cash_from_financing_cents },
    { section: 'Summary', account_code: '', account_name: 'Net change in cash', amount_cents: d.net_change_in_cash_cents },
    { section: 'Summary', account_code: '', account_name: 'Cash at start', amount_cents: d.cash_start_cents },
    { section: 'Summary', account_code: '', account_name: 'Cash at end', amount_cents: d.cash_end_cents },
    { section: 'Summary', account_code: '', account_name: 'Reconciliation gap', amount_cents: d.reconciliation_gap_cents },
  ];
  const columns: ReadonlyArray<CsvColumn<StatementRow>> = [
    { header: 'Section', value: (r) => r.section },
    { header: 'Account code', value: (r) => r.account_code },
    { header: 'Account name', value: (r) => r.account_name },
    { header: 'Amount', value: (r) => centsToDollars(r.amount_cents) },
    { header: 'Currency', value: () => d.currency },
  ];
  return rowsToCsv(rows, columns);
}

function trialBalanceCsv(d: TrialBalanceResponse): string {
  const columns: ReadonlyArray<CsvColumn<TrialBalanceRow>> = [
    { header: 'Account code', value: (r) => r.account_code },
    { header: 'Account name', value: (r) => r.account_name },
    { header: 'Kind', value: (r) => r.kind },
    { header: 'Normal side', value: (r) => r.normal_side },
    { header: 'Debits', value: (r) => centsToDollars(r.debit_total_cents) },
    { header: 'Credits', value: (r) => centsToDollars(r.credit_total_cents) },
    { header: 'Balance', value: (r) => centsToDollars(r.balance_cents) },
    { header: 'Currency', value: (r) => r.currency },
  ];
  return rowsToCsv(d.rows, columns);
}

function readme(range: MonthRange): string {
  return [
    `Monthly close — ${range.month}`,
    '',
    `Period: ${range.from} through ${range.to} (inclusive).`,
    `Generated: ${appNow().toISOString()}`,
    '',
    'Files:',
    '  trial-balance.csv     GET /api/ledger/trial-balance?as_of=' + range.to,
    '  income-statement.csv  GET /api/ledger/income-statement?from=' +
      range.from +
      '&to=' +
      range.to,
    '  balance-sheet.csv     GET /api/ledger/balance-sheet?as_of=' + range.to,
    '  cash-flow.csv         GET /api/ledger/cash-flow?from=' +
      range.from +
      '&to=' +
      range.to,
    '',
    'Amounts are USD dollars (2 decimal places). Accounts with zero',
    "balance for the period are omitted from IS/BS/CF but present in",
    'the trial balance for completeness.',
    '',
    'Data source: BOSS ledger (boss-ledger crate) — regenerate this',
    'package by re-querying the endpoints above. Numbers are',
    'deterministic for any closed period (all source periods locked).',
  ].join('\n');
}

export type CloseResult = Readonly<{
  blob: Blob;
  filename: string;
  /// Non-fatal warnings: which reports failed to load. The ZIP still
  /// ships; missing reports are omitted and called out in the README.
  warnings: ReadonlyArray<string>;
}>;

/// Fetch the four statements in parallel and bundle into a ZIP. Any
/// report that errors out is omitted — the others still go in, with
/// the README listing what's missing. Failing silently would be
/// worse than flagging a partial package.
export async function buildMonthlyClosePackage(
  month: string,
): Promise<CloseResult> {
  const range = monthRange(month);
  const [tb, is, bs, cf] = await Promise.all([
    loadTrialBalance(range.to),
    loadIncomeStatement(range.from, range.to),
    loadBalanceSheet(range.to),
    loadCashFlow(range.from, range.to),
  ]);

  const files: ZipFile[] = [];
  const warnings: string[] = [];

  if (tb) files.push({ name: 'trial-balance.csv', content: trialBalanceCsv(tb) });
  else warnings.push('trial-balance failed to load');
  if (is) files.push({ name: 'income-statement.csv', content: incomeStatementCsv(is) });
  else warnings.push('income-statement failed to load');
  if (bs) files.push({ name: 'balance-sheet.csv', content: balanceSheetCsv(bs) });
  else warnings.push('balance-sheet failed to load');
  if (cf) files.push({ name: 'cash-flow.csv', content: cashFlowCsv(cf) });
  else warnings.push('cash-flow failed to load');

  const readmeContent =
    readme(range) +
    (warnings.length > 0
      ? '\n\nWarnings:\n' + warnings.map((w) => '  - ' + w).join('\n') + '\n'
      : '\n');
  files.unshift({ name: 'README.txt', content: readmeContent });

  const blob = buildZip(files);
  const filename = `monthly-close-${range.month}-${dateStamp()}.zip`;
  return { blob, filename, warnings };
}

/// Trigger the browser download. Same pattern as `downloadCsv` but for
/// the composed ZIP blob.
export function downloadZip(filename: string, blob: Blob): void {
  const url = URL.createObjectURL(blob);
  const a = document.createElement('a');
  a.href = url;
  a.download = filename;
  document.body.appendChild(a);
  a.click();
  document.body.removeChild(a);
  setTimeout(() => URL.revokeObjectURL(url), 0);
}
