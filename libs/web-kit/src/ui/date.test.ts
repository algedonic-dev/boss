// Unit tests for the shared date formatters. Run via `bun test`.

import { describe, expect, test } from 'bun:test';
import { formatDate, formatDateTime, formatRelative } from './date';

describe('formatDate', () => {
  test('YYYY-MM-DD renders as en-US short form', () => {
    expect(formatDate('2026-08-22')).toBe('Aug 22, 2026');
    expect(formatDate('2026-01-05')).toBe('Jan 5, 2026');
    expect(formatDate('2025-12-31')).toBe('Dec 31, 2025');
  });

  test('full ISO timestamps keep the calendar date they carry', () => {
    // String-parsed, not Date-parsed: a timestamp's date part must
    // never shift a day under the viewer's timezone offset.
    expect(formatDate('2026-08-22T23:30:00Z')).toBe('Aug 22, 2026');
    expect(formatDate('2026-01-01T00:00:00Z')).toBe('Jan 1, 2026');
  });

  test('non-date input passes through unchanged', () => {
    expect(formatDate('n/a')).toBe('n/a');
    expect(formatDate('')).toBe('');
    expect(formatDate('2026-13-40')).toBe('2026-13-40');
  });
});

describe('formatDateTime', () => {
  test('ISO timestamp renders date + 24h time', () => {
    expect(formatDateTime('2026-08-22T14:30:00Z', { timeZone: 'UTC' })).toBe(
      'Aug 22, 2026, 14:30',
    );
  });

  test('midnight renders 00:xx, not 24:xx', () => {
    expect(formatDateTime('2026-08-22T00:05:00Z', { timeZone: 'UTC' })).toBe(
      'Aug 22, 2026, 00:05',
    );
  });

  test('non-timestamp input passes through unchanged', () => {
    expect(formatDateTime('soon', { timeZone: 'UTC' })).toBe('soon');
  });
});

describe('formatRelative', () => {
  // Behavior absorbed byte-for-byte from the two identical `daysAgo`
  // helpers in apps/web/src/accounts/{NotesPanel,ActivityTimeline}.svelte
  // — same buckets, same labels, so those panels can adopt this
  // helper later with zero visual change.
  const now = new Date('2026-08-22T00:00:00Z');

  test('same day (and partial days) render as today', () => {
    expect(formatRelative('2026-08-22T00:00:00Z', now)).toBe('today');
    expect(formatRelative('2026-08-21T12:00:00Z', now)).toBe('today');
  });

  test('future dates clamp to today', () => {
    expect(formatRelative('2026-09-01T00:00:00Z', now)).toBe('today');
  });

  test('whole days under a month render as Nd', () => {
    expect(formatRelative('2026-08-21T00:00:00Z', now)).toBe('1d');
    expect(formatRelative('2026-08-19T00:00:00Z', now)).toBe('3d');
    expect(formatRelative('2026-07-24T00:00:00Z', now)).toBe('29d');
  });

  test('30..364 days render as floor(d/30) months', () => {
    expect(formatRelative('2026-07-23T00:00:00Z', now)).toBe('1mo'); // 30d
    expect(formatRelative('2026-06-24T00:00:00Z', now)).toBe('1mo'); // 59d
    expect(formatRelative('2026-06-23T00:00:00Z', now)).toBe('2mo'); // 60d
    expect(formatRelative('2025-08-23T00:00:00Z', now)).toBe('12mo'); // 364d
  });

  test('365+ days render as floor(d/365) years', () => {
    expect(formatRelative('2025-08-22T00:00:00Z', now)).toBe('1y'); // 365d
    expect(formatRelative('2024-06-01T00:00:00Z', now)).toBe('2y');
  });

  test('bare YYYY-MM-DD dates work too', () => {
    expect(formatRelative('2026-08-19', now)).toBe('3d');
  });
});
