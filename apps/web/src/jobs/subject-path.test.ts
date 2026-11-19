// Where clicking a Subject lands. A kind with a page of its own (a
// purchase order, a vendor) opens that page; every other kind is drawn
// as text (null), because the jobs list its packets used to open went
// with step 1 of design 01fa5bdf (review df7aeb9c).
//
// None may go to a workflow kind, because a kind is tenant data and
// this file is shared by every instance (backlog 423a531d: the campaign
// branch returned `/jobs?kind=marketing-motion`). And none may go to
// `'#'` (backlog 4af37dd8, page audit 473f4f92 GAP 2): `href('#')` is
// `/#`, which parses as `/`, so a Subject click landed on Home — for
// every open packet, all of them `custom`. Null is the honest answer
// for a kind with no page.
import { describe, expect, it } from 'bun:test';
import { subjectPath } from './types';
import { parseRoute, type Route } from '../router';
import type { Subject } from './types';

const subject = (subject_kind: string, id: string): Subject =>
  ({ subject_kind, id }) as unknown as Subject;

describe('subjectPath', () => {
  it('opens a page only for a kind that has one, to a route the router serves', () => {
    const cases: ReadonlyArray<[string, string, Route['kind']]> = [
      // Finance's pages live at Finance's address (backlog c7ef4728).
      ['purchase_order', '/finance/purchase-orders/x', 'po'],
      ['vendor', '/finance/vendors/x', 'vendor'],
    ];
    for (const [k, want, route] of cases) {
      const p = subjectPath(subject(k, 'x'));
      expect(p).toBe(want);
      expect(parseRoute(p!).kind).toBe(route);
    }
  });

  it('url-encodes the id', () => {
    expect(subjectPath(subject('vendor', 'v a/b'))).toBe('/finance/vendors/v%20a%2Fb');
  });

  it('answers null for every kind with no page, never a path to the retired list or to Home', () => {
    // An asset's, an account's and an employee's /ux pages went with
    // step 1 of design 01fa5bdf, and so did the list a pageless kind
    // landed on. `recipe` is a brewery-tenant row and `department` a
    // platform one, neither named in the switch.
    for (const k of ['asset', 'account', 'employee', 'campaign', 'custom', 'recipe', 'department']) {
      expect({ kind: k, path: subjectPath(subject(k, 'x')) }).toEqual({ kind: k, path: null });
    }
  });
});
