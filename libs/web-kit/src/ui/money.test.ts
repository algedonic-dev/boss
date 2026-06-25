// Unit tests for the money formatter. Run via `bun test`.

import { describe, expect, test } from 'bun:test';
import { formatMoney } from './money';

describe('formatMoney precision', () => {
  test("auto: amounts >= $100 round to whole units", () => {
    expect(formatMoney({ amount_cents: 19_200_00, currency: 'USD' })).toBe('$19,200');
    expect(formatMoney({ amount_cents: 12_345, currency: 'USD' })).toBe('$123');
    expect(formatMoney({ amount_cents: 10_000, currency: 'USD' })).toBe('$100');
  });

  test('auto: amounts under $100 keep cents', () => {
    expect(formatMoney({ amount_cents: 9_999, currency: 'USD' })).toBe('$99.99');
    expect(formatMoney({ amount_cents: 4_250, currency: 'USD' })).toBe('$42.50');
    expect(formatMoney({ amount_cents: 0, currency: 'USD' })).toBe('$0.00');
  });

  test("'cents' precision always shows two decimals", () => {
    expect(formatMoney(
      { amount_cents: 19_200_00, currency: 'USD' },
      { precision: 'cents' },
    )).toBe('$19,200.00');
    expect(formatMoney(
      { amount_cents: 12_345, currency: 'USD' },
      { precision: 'cents' },
    )).toBe('$123.45');
  });

  test("'whole' precision strips decimals at any magnitude", () => {
    expect(formatMoney(
      { amount_cents: 19_200_00, currency: 'USD' },
      { precision: 'whole' },
    )).toBe('$19,200');
    expect(formatMoney(
      { amount_cents: 4_250, currency: 'USD' },
      { precision: 'whole' },
    )).toBe('$43');
    expect(formatMoney(
      { amount_cents: 99, currency: 'USD' },
      { precision: 'whole' },
    )).toBe('$1');
  });

  test('accounting style still keeps cents under auto', () => {
    expect(formatMoney(
      { amount_cents: 19_200_00, currency: 'USD' },
      { style: 'accounting' },
    )).toBe('19,200.00 USD');
    expect(formatMoney(
      { amount_cents: -19_200_00, currency: 'USD' },
      { style: 'accounting' },
    )).toBe('(19,200.00) USD');
  });

  test('accounting respects an explicit whole precision request', () => {
    expect(formatMoney(
      { amount_cents: 19_200_00, currency: 'USD' },
      { style: 'accounting', precision: 'whole' },
    )).toBe('19,200 USD');
  });

  test('zero-decimal currency (JPY) is unaffected by precision', () => {
    expect(formatMoney({ amount_cents: 1_234, currency: 'JPY' })).toBe('¥1,234 JPY');
    expect(formatMoney(
      { amount_cents: 1_234, currency: 'JPY' },
      { precision: 'cents' },
    )).toBe('¥1,234 JPY');
  });

  test('negatives round consistently with positives', () => {
    expect(formatMoney({ amount_cents: -19_200_00, currency: 'USD' })).toBe('-$19,200');
    expect(formatMoney({ amount_cents: -4_250, currency: 'USD' })).toBe('-$42.50');
  });
});
