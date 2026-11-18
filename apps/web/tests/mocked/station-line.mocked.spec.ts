// THE STATION LINE (design 08d1530e car B, approved by David 2026-11-12;
// backlog 9d0b0944), against MOCKED live reads: the tree, every open
// claim, the protocol rows, the open count per protocol, and the chosen
// protocol's open packets. The pure rules are pinned in
// src/it/tree/station-line.test.ts; this pins the page — it opens on the
// owned protocol with the most open packets, the others are count chips,
// each stop is a station box whose cells name their packets and carry
// the claim's mark, the gates seat their packets from the packet metadata
// with a machine queue of their own, a packet whose step names no station is
// in the tray, and a failed read draws the red rail.

import { expect, test, type Page, type Route } from './_test';
import {
  claimsRead, gateRunPacket, heldClaim, kindsRead, openPacket, packetsRead, queueRead, stationList, stationLoad, treeRead, workflowsRead,
} from '../fixtures/station-tree';
import { STATIONS_LOAD, installSmokeMocks } from './_smokeMocks';

const STATIONS = /\/api\/stations(\?|$)/;
const QUEUE = /\/api\/stations\/[^/]+\/queue$/;
const CLAIMS = /\/api\/claims(\?|$)/;
const WORKFLOWS = /\/api\/workflows(\?|$)/;
const KINDS = /\/api\/jobs\/kinds(\?|$)/;
const PACKETS = /\/api\/jobs\?kind=/;

const json = (r: Route, b: unknown, status = 200) =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(b) });

type Opts = Readonly<{ packets?: 'ok' | 'down' }>;

async function mocks(page: Page, o: Opts = {}): Promise<void> {
  await installSmokeMocks(page);
  await page.route(STATIONS, (r) => {
    const under = new URL(r.request().url()).searchParams.get('under');
    if (under === null) return json(r, stationList());
    return json(r, treeRead(under.startsWith('id-') ? under.slice(3) : under));
  });
  await page.route(STATIONS_LOAD, (r) => json(r, stationLoad()));
  await page.route(QUEUE, (r) => json(r, queueRead('engineering')));
  // j-2's review step is claimed by a person standing outside the tree:
  // the line reads every claim, not the page's subtree.
  await page.route(CLAIMS, (r) => json(r, claimsRead([
    heldClaim('j-2', null, { step_id: 'j-2-review', executor_kind: 'person', holder: 'emp-david' }),
  ])));
  await page.route(WORKFLOWS, (r) => json(r, workflowsRead()));
  await page.route(KINDS, (r) => json(r, kindsRead([
    { kind: 'ship-a-change', open: 3 },
    { kind: 'gate-run', open: 3 },
    { kind: 'pr-train', open: 1 },
  ])));
  await page.route(PACKETS, (r) => {
    if (o.packets === 'down') return r.fulfill({ status: 503, body: 'jobs read deadline exhausted' });
    const kind = new URL(r.request().url()).searchParams.get('kind');
    if (kind === 'gate-run') {
      return json(r, packetsRead([
        gateRunPacket('g-1', 'Gate: feat/a-seat-names-its-lease', 'lane-1'),
        gateRunPacket('g-2', 'Gate: fix/receipts-carry-bytes', 'queued'),
        gateRunPacket('g-3', 'Gate: feat/no-reservation-yet', null),
      ]));
    }
    return json(r, packetsRead([
      pinned(openPacket('j-1', 'Scope the seat lease', 'scope')),
      pinned(openPacket('j-2', 'Gate receipts carry their bytes', 'review')),
      pinned(openPacket('j-3', 'A ready step names its station', 'gate')),
    ]));
  });
}

/** A packet pinned to the row's version, as the list serves `workflow_version`. */
const pinned = (p: ReturnType<typeof openPacket>) => ({ ...p, workflow_version: 1 });

test('the line opens on the busiest owned protocol, each stop a station box, and a packet whose step names no station is in the tray', async ({ page }, testInfo) => {
  await mocks(page);
  await page.goto('/it/tree/engineering');
  const chips = page.locator('[data-protocol-chips] [data-chip]');
  // ship-a-change and gate-run tie on 3; the chooser's own order breaks it.
  await expect(chips).toHaveText(['ship-a-change3', 'gate-run3', 'pr-train1']);
  await expect(page.locator('[data-chip="ship-a-change"]')).toHaveAttribute('aria-pressed', 'true');
  await expect(page.locator('[data-line="ship-a-change"]')).toBeVisible();
  await expect(page.locator('[data-stop="gate"]')).toHaveAttribute('data-station', 'gates');
  await expect(page.locator('[data-stop="review"]')).toHaveAttribute('data-station', 'loading-dock');
  await expect(page.locator('[data-stop="scope"]')).toHaveAttribute('data-station', '');
  // The claim's executor_kind is the mark: a person, not the robot by default.
  const held = page.locator('[data-stop="review"] [data-cell="j-2"]');
  await expect(held).toBeVisible();
  await expect(held.locator('[data-mark]')).toHaveText('●');
  // Placed by no guess: the scope packet stands in the tray, by step and version.
  await expect(page.locator('[data-tray-row="scope"]')).toContainText('at scope · v1: its step names no station');
  await expect(page.locator('[data-stop="scope"] [data-stop-tray]')).toContainText('1 here');
  // The motion band's chooser is gone: the line is the page.
  await expect(page.locator('[data-protocol-chooser]')).toHaveCount(0);
  await page.locator('[data-band="line"]').screenshot({ path: testInfo.outputPath('station-line-ship-a-change.png') });
});

test('the gates seat their packets from the packet metadata, with a machine queue of their own and an admission column before it', async ({ page }, testInfo) => {
  await mocks(page);
  await page.goto('/it/tree/engineering');
  await page.locator('[data-chip="gate-run"]').click();
  const gates = page.locator('[data-stop="record-verdict"]');
  await expect(gates).toHaveAttribute('data-station', 'gates');
  await expect(gates.locator('[data-cell="g-1"]')).toHaveAttribute('data-cell-label', 'seated');
  await expect(gates.locator('[data-cell-label="a seat"]')).toHaveCount(1);
  await expect(gates.locator('[data-machine-queue]')).toHaveAttribute('data-machine-queue', '1');
  await expect(gates.locator('[data-in]')).toHaveAttribute('data-in', '1');
  await expect(gates.locator('[data-in-tag="g-3"]')).toBeVisible();
  await expect(gates.locator('[data-box-notes]')).toContainText('a seat here is read from packet metadata');
  await page.locator('[data-band="line"]').screenshot({ path: testInfo.outputPath('station-line-gates.png') });
});

test('a failed packets read draws the red rail and no packet', async ({ page }) => {
  await mocks(page, { packets: 'down' });
  await page.goto('/it/tree/engineering');
  await expect(page.locator('[data-line-failed="packets"]')).toContainText('503');
  await expect(page.locator('[data-line-failed="packets"]')).toHaveClass(/load-failed/);
  await expect(page.locator('[data-cell]')).toHaveCount(0);
});
