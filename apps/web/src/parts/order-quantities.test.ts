import { expect, test } from 'bun:test';
import { orderQuantities } from './order-quantities';

const order = (status = 'submitted', qty: unknown = 3) => ({ id: 'po1', status, lines: [{ part_sku: 'SKU', qty }] });
test('known quantities conserve open lines, including tenant-added statuses', () => {
  const rows = [order('tenant-review'), { ...order(), id: 'po2' }, { ...order('received'), id: 'po3' }, { ...order('closed'), id: 'po4' }];
  const before = JSON.stringify(rows);
  expect(orderQuantities('/orders', rows).get('SKU')).toBe(6);
  expect(JSON.stringify(rows)).toBe(before);
  expect(orderQuantities('/orders', []).size).toBe(0);
  expect(orderQuantities('/orders', { data: [order('draft', 0)] }).get('SKU')).toBe(0);
});
for (const qty of [null, '3', -1, 1.5, 4294967296, NaN, Infinity]) {
  test(`malformed quantity ${String(qty)} refuses the entire result`, () => {
    expect(() => orderQuantities('/orders', [order(), { ...order('submitted', qty), id: 'po2' }])).toThrow('/orders: HTTP 200');
  });
}
for (const row of [null, {}, { ...order(), lines: undefined }, { ...order(), status: '' }, { ...order(), id: '' }, { ...order(), lines: [{ part_sku: '', qty: 2 }] }]) {
  test(`malformed consumed row ${JSON.stringify(row)} is not a clean zero`, () => {
    expect(() => orderQuantities('/orders', [row])).toThrow('/orders: HTTP 200');
  });
}
test('duplicate identities cannot double count and invalid envelopes cannot claim zero', () => {
  expect(() => orderQuantities('/orders', [order(), order()])).toThrow('duplicate');
  expect(() => orderQuantities('/orders', { error: 'bad' })).toThrow('/orders: HTTP 200');
});
