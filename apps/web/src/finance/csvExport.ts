// Tiny CSV export utility — generate + download in the browser.
//
// No backend round-trip; the JSON the report tabs already fetched
// is enough. Keeps "make the auditor happy" exports zero-backend-
// change: the caller hands us column headers + row values, we
// quote what needs quoting and trigger the browser download.

type CellValue = string | number | null | undefined;

import { appToday } from '@boss/web-kit/sim-clock';

export type CsvColumn<T> = Readonly<{
  header: string;
  value: (row: T) => CellValue;
}>;

/// Escape a single cell per RFC 4180: wrap in double-quotes only when
/// the content contains a delimiter / quote / newline, and double any
/// embedded quotes. null / undefined becomes an empty cell.
function escapeCell(v: CellValue): string {
  if (v === null || v === undefined) return '';
  const s = typeof v === 'number' ? String(v) : v;
  if (s.includes(',') || s.includes('"') || s.includes('\n') || s.includes('\r')) {
    return `"${s.replace(/"/g, '""')}"`;
  }
  return s;
}

export function rowsToCsv<T>(
  rows: ReadonlyArray<T>,
  columns: ReadonlyArray<CsvColumn<T>>,
): string {
  const header = columns.map(c => escapeCell(c.header)).join(',');
  const body = rows.map(r =>
    columns.map(c => escapeCell(c.value(r))).join(','),
  );
  // Prepend BOM so Excel opens UTF-8 CSVs correctly without prompting
  // for encoding; \r\n line endings match the RFC more strictly (Excel
  // still accepts \n but \r\n is friendliest).
  return '\ufeff' + [header, ...body].join('\r\n') + '\r\n';
}

/// Trigger a browser download for the given CSV content + filename.
export function downloadCsv(filename: string, content: string): void {
  const blob = new Blob([content], { type: 'text/csv;charset=utf-8' });
  const url = URL.createObjectURL(blob);
  const a = document.createElement('a');
  a.href = url;
  a.download = filename;
  document.body.appendChild(a);
  a.click();
  document.body.removeChild(a);
  // revoke on the next tick so Safari has time to start the download.
  setTimeout(() => URL.revokeObjectURL(url), 0);
}

export function exportRows<T>(
  filename: string,
  rows: ReadonlyArray<T>,
  columns: ReadonlyArray<CsvColumn<T>>,
): void {
  downloadCsv(filename, rowsToCsv(rows, columns));
}

/// Integer-cents → fixed-decimal string (e.g. 12345 → "123.45"). CSV
/// consumers (Excel, pandas) prefer decimal dollars over raw cents.
export function centsToDollars(cents: number): string {
  const sign = cents < 0 ? '-' : '';
  const abs = Math.abs(cents);
  const whole = Math.floor(abs / 100);
  const frac = abs % 100;
  return `${sign}${whole}.${frac.toString().padStart(2, '0')}`;
}

/// YYYY-MM-DD stamp for report filenames. Falls back to "today" when
/// no as-of date is supplied.
export function dateStamp(date?: string | null): string {
  if (date && date.length >= 10) return date.slice(0, 10);
  return appToday();
}

/// Trigger the browser's print dialog scoped to the nearest
/// `.finance-print-area` ancestor. Adds `boss-printing` to `<body>`
/// before `window.print()` so the print stylesheet can hide nav,
/// tabs, and buttons; the `afterprint` handler removes the class so
/// the screen view snaps back to normal. Users save-as-PDF through
/// the browser's standard print dialog — no PDF library required.
export function printReport(): void {
  const body = document.body;
  body.classList.add('boss-printing');
  const handler = (): void => {
    body.classList.remove('boss-printing');
    window.removeEventListener('afterprint', handler);
  };
  window.addEventListener('afterprint', handler);
  window.print();
}
