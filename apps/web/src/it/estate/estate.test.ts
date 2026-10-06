import { afterEach, describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import {
  comparisonVerdict,
  DEV_DOOR_HOST,
  devDoorSteps,
  ESTATE_LOOPS,
  fetchEstate,
  HOST_COMPARISONS_READ,
  hostCoverText,
  hostLines,
  hostPageIsWhole,
  hostPlan,
  hostSeriesRead,
  CLUSTER_COMPARISON_READ,
  CLUSTER_OBSERVATIONS_READ,
  freshnessText,
  latestComparison,
  latestPerHost,
  loopAge,
  loopHost,
  loopPlan,
  loopQueries,
  LOOP_OK_OUTCOMES,
  missingHostText,
  OPS_REQUEST_KIND,
  OPS_RUNNER_ROLE,
  parseComparisons,
  parseHostComparisons,
  parseLoopPackets,
  registryFailure,
  parseNodes,
  parseObservations,
  parseSeriesPage,
  seriesAbsentText,
  seriesFreshness,
  STALE_MIN_CADENCE_S,
  STALE_MIN_OBSERVATIONS,
  STALE_MULTIPLIER,
  unitsVerdict,
  alarmCoverText,
  alarmsOn,
  MACHINE_FIELDS,
  machineDrift,
  machineSight,
  machineValue,
  OPEN_ALARMS_READ,
  parseAlarms,
  seenCell,
  CLUSTER_CONVERGE_KIND,
  parseZoneOpen,
  parseZoneReadings,
  tunnelLine,
  ZONE_KIND,
  ZONE_OPEN_READ,
  ZONE_READINGS_READ,
  zoneAlarms,
  zoneVerdict,
  parseVolumeReadings,
  volumeAlarms,
  volumeLine,
  VOLUMES_READ,
  VOLUMES_SCOPE,
  type Comparison,
  type EstateNode,
  type EstateState,
  type LoopRow,
} from './estate';

const realFetch = globalThis.fetch;
afterEach(() => {
  globalThis.fetch = realFetch;
});

function node(over: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    id: 'w-1', label: 'Worker 1', address: '10.20.0.14', role: 'talos-worker',
    cpu: 32, memory_gb: 63, disk_gb: 929, notes: 'the build node', retired: false,
    ...over,
  };
}

function obsEvent(scope: string, observed_at: string, nodes: unknown[] = [{ id: 'x' }]): unknown {
  return { payload: { scope, observed_at, observer: 'boss-estate-observe', nodes } };
}

describe('parseNodes', () => {
  test('accepts both bare arrays and {data: []} envelopes, and carries declared capacity through', () => {
    const rows = parseNodes({ data: [node()] });
    expect(rows).toHaveLength(1);
    expect(rows[0]?.cpu).toBe(32);
    expect(rows[0]?.memory_gb).toBe(63);
    expect(parseNodes([node()])[0]?.id).toBe('w-1');
  });

  test('a row without an id is a parse failure, not a silently dropped machine', () => {
    // A machine vanishing from the render because a field went missing
    // is exactly the estate's failure story (w-1 invisible for five
    // days) — refuse loudly instead.
    expect(() => parseNodes([{ role: 'talos-worker' }])).toThrow();
  });
  test('declared roles ride along as a set; a node declaring none reads empty', () => {
    const [gcp, w1] = parseNodes([
      { ...node(), id: 'boss-gcp', role: 'bastion', roles: ['legacy-stack', 'wireguard-bastion'] },
      node(),
    ]);
    expect(gcp?.roles).toEqual(['legacy-stack', 'wireguard-bastion']);
    expect(w1?.roles).toEqual([]);
  });
});

// ONE SCOPED READ PER RENDERED SERIES (backlog 75027a93; page audit
// 2cff1d6e, GAP 4). The page read one unscoped page of 20 observation
// rows across every scope — measured 2026-09-27 17:14Z it held 11
// host-units rows, 6 door, 2 host (both forge's) and 1 kubernetes-nodes,
// of 6106, and boss-gcp's daily host reading at 10:25Z was not among
// them, so boss-gcp had no host line at all. A limit is not a filter:
// each series is its own read, and "never recorded" is said only when
// that read's total is 0.
describe('the series reads', () => {
  test('the cluster series and each host\'s two series are read scoped, the host ones by host', () => {
    expect(CLUSTER_OBSERVATIONS_READ).toBe('/api/estate/observations?scope=kubernetes-nodes&limit=10');
    expect(CLUSTER_COMPARISON_READ).toBe('/api/estate/comparisons?scope=kubernetes-nodes&limit=1');
    expect(hostSeriesRead('host', 'boss-gcp')).toBe('/api/estate/observations?scope=host&host=boss-gcp&limit=10');
    expect(hostSeriesRead('host-units', 'a b')).toBe('/api/estate/observations?scope=host-units&host=a%20b&limit=10');
  });

  test('a series page keeps only its own rows, and the total the server counted', () => {
    // A jobs API that predates ?host= answers the whole scope to every
    // host's read (the alarm's own guard, estate_alarm.rs); a neighbour's
    // rows are not this host's series.
    const page = parseSeriesPage({
      data: [
        obsEvent('host', '2026-09-27T17:05:45Z', [{ id: 'forge', disk_free_gb: 210 }]),
        obsEvent('host', '2026-09-27T10:25:01Z', [{ id: 'boss-gcp', disk_free_gb: 13 }]),
        obsEvent('host-units', '2026-09-27T10:24:00Z', [{ id: 'boss-gcp' }]),
      ],
      total: 999,
    }, 'host', 'boss-gcp');
    expect(page.rows.map((r) => [r.nodes[0]?.id, r.observed_at])).toEqual([['boss-gcp', '2026-09-27T10:25:01Z']]);
    expect(page.total).toBe(999);
    expect(page.returned).toBe(3);
    // The cluster's series names no host: only the scope narrows it.
    const cluster = parseSeriesPage([obsEvent('kubernetes-nodes', '2026-09-27T17:00:02Z', [{ id: 'cp-1' }])], 'kubernetes-nodes', null);
    expect(cluster.rows).toHaveLength(1);
    expect(cluster.total).toBeNull();
  });

  test('"never recorded" only when the series\' own read counted zero; otherwise the absence is the read\'s', () => {
    expect(seriesAbsentText(parseSeriesPage({ data: [], total: 0 }, 'host', 'forge'))).toBe('no observation recorded yet');
    expect(seriesAbsentText(parseSeriesPage({
      data: [obsEvent('host', '2026-09-27T17:05:45Z', [{ id: 'forge' }])], total: 989,
    }, 'host', 'boss-gcp'))).toBe('none of the 1 row this read returned is this series\' (the read counted 989)');
    expect(seriesAbsentText(parseSeriesPage([], 'host', 'forge'))).toBe(
      'none of the 0 rows this read returned is this series\', and the read did not say how many there are',
    );
  });

  test('the hosts read are every live declared host outside the cluster plus every host that compared', () => {
    const nodes = {
      kind: 'ready' as const,
      data: parseNodes([
        node({ id: 'forge', role: 'forge' }),
        node({ id: 'boss-gcp', role: 'bastion' }),
        node({ id: 'w-1', role: 'talos-worker' }),
        node({ id: 'old-host', role: 'forge', retired: true }),
      ]),
    };
    const compared = { kind: 'ready' as const, data: parseHostComparisons({ data: [hostCmp('mystery-box', '2026-09-27T10:00:00Z')], total: 1 }) };
    const failed = { kind: 'failed' as const, error: 'HTTP 503' };
    expect(hostPlan(nodes, compared)).toEqual({ known: true, hosts: ['boss-gcp', 'forge', 'mystery-box'] });
    expect(hostPlan(failed, compared)).toEqual({ known: true, hosts: ['mystery-box'] });
    expect(hostPlan(nodes, failed)).toEqual({ known: true, hosts: ['boss-gcp', 'forge'] });
    // Neither source answered: no host is known, which is not "no host".
    expect(hostPlan(failed, failed)).toEqual({ known: false, hosts: [] });
  });
});

// A SERIES IS JUDGED BY ITS OWN CADENCE (backlog e1eb34bc; page audit
// 2cff1d6e, GAP 8). The age was grey relative text — "today" for a
// three-minute-old row and a three-hour-old one alike — while the
// estate alarm files `unobserved:*` when a series is quiet past 3x its
// own measured cadence. boss-gcp's host observer is daily (10:25Z,
// operator-measured 2026-09-23) and forge's every fifteen minutes, so
// one global age would read boss-gcp stale every afternoon.
describe('seriesFreshness', () => {
  const now = new Date('2026-09-27T17:15:00Z');
  const rows = (...stamps: string[]) => parseObservations(stamps.map((s) => obsEvent('host', s, [{ id: 'h' }])));

  test('inside three of its cadences a series is fresh, and says its cadence', () => {
    const f = seriesFreshness(rows('2026-09-27T17:05:00Z', '2026-09-27T16:50:00Z', '2026-09-27T16:35:00Z'), now);
    expect(f).toEqual({ state: 'fresh', ageS: 600, cadenceS: 900 });
    expect(freshnessText(f!)).toBe('10m ago · every 15m');
  });

  test('a daily series seven hours old is fresh — judged by ITS cadence, not the forge\'s', () => {
    const f = seriesFreshness(rows('2026-09-27T10:25:00Z', '2026-09-26T10:25:00Z', '2026-09-25T10:25:00Z'), now);
    expect(f?.state).toBe('fresh');
    expect(freshnessText(f!)).toBe('6.8h ago · every 24h');
  });

  test('past three of its cadences a series is stale, and says so against the cadence', () => {
    const f = seriesFreshness(rows('2026-09-27T16:15:00Z', '2026-09-27T16:10:00Z', '2026-09-27T16:05:00Z'), now);
    expect(f).toEqual({ state: 'stale', ageS: 3600, cadenceS: 300 });
    expect(freshnessText(f!)).toBe('1h ago — quiet past 3× its 5m cadence');
  });

  test('exactly three cadences is not yet stale — the alarm\'s strict greater-than', () => {
    expect(seriesFreshness(rows('2026-09-27T17:00:00Z', '2026-09-27T16:55:00Z', '2026-09-27T16:50:00Z'), now)?.state).toBe('fresh');
  });

  test('the cadence is the median gap, so one missed firing does not stretch it', () => {
    // Gaps 5, 5, 20 minutes: the median (upper, as the alarm takes it)
    // is 5, not the mean.
    const f = seriesFreshness(rows(
      '2026-09-27T17:10:00Z', '2026-09-27T17:05:00Z', '2026-09-27T17:00:00Z', '2026-09-27T16:40:00Z',
    ), now);
    expect(f?.cadenceS).toBe(300);
  });

  test('fewer than three readings is no measured cadence, and no verdict is invented', () => {
    const f = seriesFreshness(rows('2026-09-20T17:00:00Z', '2026-09-20T16:55:00Z'), now);
    expect(f).toEqual({ state: 'unmeasured', ageS: 7 * 86400 + 900, cadenceS: null });
    expect(freshnessText(f!)).toBe('168.3h ago · cadence not yet measured');
    expect(seriesFreshness([], now)).toBeNull();
  });

  test('back-to-back manual posts cannot fake a fast cadence — the alarm\'s 60 s floor', () => {
    const f = seriesFreshness(rows('2026-09-27T17:13:00Z', '2026-09-27T17:12:59Z', '2026-09-27T17:12:58Z'), now);
    expect(f).toEqual({ state: 'fresh', ageS: 120, cadenceS: 60 });
  });

  // CLAUDE.md §9a: the threshold lives in the alarm AND here, so it is
  // pinned — the page must call stale exactly what the alarm files.
  test('the multiplier, the minimum readings and the cadence floor are the alarm\'s own', () => {
    const alarm = readFileSync(
      new URL('../../../../../crates/orchestrators/boss-dispatcher-handlers/src/handlers/estate_alarm.rs', import.meta.url),
      'utf8',
    );
    expect(alarm).toContain(`const STALE_MULTIPLIER: i64 = ${STALE_MULTIPLIER};`);
    expect(alarm).toContain(`const STALE_MIN_OBSERVATIONS: usize = ${STALE_MIN_OBSERVATIONS};`);
    expect(alarm).toContain(`const STALE_MIN_CADENCE_S: i64 = ${STALE_MIN_CADENCE_S};`);
  });
});

// UNIT HEALTH (backlog d5efb80d; page audit 2cff1d6e, GAP 3). host-units
// was 12 of the 20 rows the page fetched and it drew none of them: at
// 16:22Z on 2026-09-23 boss-gcp had 15 units watched, 0 unhealthy, and
// a `units_unhealthy` finding would have been invisible here. The
// observer's shape is observe-units.sh's: one node, carrying `units`.
describe('unitsVerdict', () => {
  const unit = (name: string, healthy: boolean) => ({ unit: name, healthy });

  test('every unit healthy reads green, with how many were watched', () => {
    expect(unitsVerdict({ id: 'boss-gcp', units: [unit('a.timer', true), unit('a.service', true)] }))
      .toEqual({ ok: true, text: '2 units watched, all healthy' });
  });

  test('an unhealthy unit is counted AND named', () => {
    expect(unitsVerdict({
      id: 'forge',
      units: [unit('boss-train.service', false), unit('boss-train.timer', true), unit('estate-observe-host.timer', false)],
    })).toEqual({ ok: false, text: '3 units watched, 2 unhealthy: boss-train.service, estate-observe-host.timer' });
  });

  test('a unit whose health was not recorded is not counted healthy', () => {
    expect(unitsVerdict({ id: 'forge', units: [{ unit: 'x.service' }] }))
      .toEqual({ ok: false, text: '1 unit watched, 1 unhealthy: x.service' });
  });

  test('a reading with no units list says the units were unread, never "all healthy"', () => {
    expect(unitsVerdict({ id: 'forge' })).toEqual({ ok: false, text: 'no units list on the reading' });
  });

  // Backlog 6647ac9a (page audit 2cff1d6e, GAP 14): the retired
  // boss-ml-api.service ran on boss-gcp from 2026-09-20 while this line
  // read "15 units watched, all healthy" — the roster is the declaration,
  // so it was outside what the observer watched. The observer now names
  // what runs outside it.
  test('a running unit nobody declared turns the line, and is named', () => {
    expect(unitsVerdict({
      id: 'boss-gcp',
      units: [unit('boss-gcp-converge.timer', true)],
      undeclared_units: [{ unit: 'boss-ml-api.service' }],
    })).toEqual({ ok: false, text: '1 unit watched, all healthy; 1 running but not declared: boss-ml-api.service' });
  });

  test('an enumeration the observer could not make is said, never read as none', () => {
    expect(unitsVerdict({
      id: 'boss-gcp',
      units: [unit('boss-gcp-converge.timer', true)],
      undeclared_unread: 'systemctl list-units exited 1',
    })).toEqual({ ok: false, text: '1 unit watched, all healthy; running units not enumerated: systemctl list-units exited 1' });
  });

  test('an empty undeclared list is the clean reading', () => {
    expect(unitsVerdict({ id: 'boss-gcp', units: [unit('a.timer', true)], undeclared_units: [] }))
      .toEqual({ ok: true, text: '1 unit watched, all healthy' });
  });
});

describe('observations and comparisons', () => {
  test('zero drift renders as the good state, with the counts said plainly', () => {
    const rows = parseComparisons([
      { payload: { scope: 'kubernetes-nodes', observed_at: '2026-08-31T10:20:01Z', counts: {
        observed: 5, participating_declared: 5,
        observed_not_declared: 0, declared_not_observed: 0, drift: 0,
      } } },
    ]);
    const c = latestComparison(rows, 'kubernetes-nodes');
    expect(c).not.toBeNull();
    const v = comparisonVerdict(c as Comparison);
    expect(v.ok).toBe(true);
    expect(v.text).toBe('5 observed, 5 declared — no drift');
  });

  test('a machine nobody declared is named, not averaged away', () => {
    const v = comparisonVerdict({
      observed_at: '', scope: 'kubernetes-nodes',
      counts: { observed: 6, participating_declared: 5, observed_not_declared: 1, declared_not_observed: 0, drift: 0 },
    });
    expect(v.ok).toBe(false);
    expect(v.text).toContain('1 in the cluster but undeclared');
  });

  test('a machine short of disk is not "no drift"', () => {
    // a520737f: the cluster comparison can now carry disk_tight, and a
    // page that renders "5 observed, 5 declared — no drift" beside a
    // build node at 90% full is the troubled-packet class — the state
    // has crossed its own alarm threshold and the surface must say so.
    const v = comparisonVerdict({
      observed_at: '', scope: 'kubernetes-nodes',
      counts: {
        observed: 5, participating_declared: 5, observed_not_declared: 0,
        declared_not_observed: 0, drift: 0, disk_tight: 1,
      },
    });
    expect(v.ok).toBe(false);
    expect(v.text).toContain('1 short of disk');
  });

  test('a node whose free space went unread is named, not counted as roomy', () => {
    // The kubelet read is best-effort, so going blind must look
    // different from having room — otherwise the instrument can fail
    // back into exactly the silence this packet reported.
    const v = comparisonVerdict({
      observed_at: '', scope: 'kubernetes-nodes',
      counts: {
        observed: 5, participating_declared: 5, observed_not_declared: 0,
        declared_not_observed: 0, drift: 0, disk_unmeasured: 2,
      },
    });
    expect(v.ok).toBe(false);
    expect(v.text).toContain('2 with no free-space reading');
  });

  test('the parser carries the disk counts through to the verdict', () => {
    // The verdict can only report what the parser keeps, and the parser
    // builds counts key by key — so the pair is tested end to end.
    const rows = parseComparisons([
      { payload: { scope: 'kubernetes-nodes', observed_at: '2026-09-10T13:45:00Z', counts: {
        observed: 5, participating_declared: 5, observed_not_declared: 0,
        declared_not_observed: 0, drift: 0, disk_tight: 1, disk_unmeasured: 1,
      } } },
    ]);
    const v = comparisonVerdict(rows[0] as Comparison);
    expect(v.ok).toBe(false);
    expect(v.text).toContain('1 short of disk');
    expect(v.text).toContain('1 with no free-space reading');
  });

  test('a comparison recorded before the disk counts existed still reads clean', () => {
    // Every row already in the series predates both keys.
    const v = comparisonVerdict({
      observed_at: '', scope: 'kubernetes-nodes',
      counts: {
        observed: 5, participating_declared: 5, observed_not_declared: 0,
        declared_not_observed: 0, drift: 0,
      },
    });
    expect(v.ok).toBe(true);
  });

  // GAPS 5 AND 6 (ea5e0e8b, c2373cc4; page audit 2cff1d6e). The verdict
  // read counts only, and the parser kept neither `not_ready` (a finding
  // with no count until ea5e0e8b) nor the dispatcher's dead-letter pair,
  // which estate.alarm raises as HARD findings. So a sick declared node
  // and a dispatcher losing dead letters both read green "no drift".

  /** A cluster comparison as compare() shapes it (estate_compare.rs). */
  const clusterRow = (counts: Record<string, number>, findings: Record<string, unknown>): unknown => ({
    payload: {
      scope: 'kubernetes-nodes', observed_at: '2026-09-27T12:00:00Z',
      counts: {
        observed: 5, participating_declared: 5, observed_not_declared: 0,
        declared_not_observed: 0, drift: 0, disk_tight: 0, disk_unmeasured: 0,
        dead_letters_unrecorded: 0, ...counts,
      },
      findings: { not_ready: [], dead_letters_unrecorded: [], dispatcher_unread: null, ...findings },
    },
  });

  test('a NotReady node is counted and named, not "no drift"', () => {
    const [c] = parseComparisons([clusterRow({ not_ready: 1 }, { not_ready: ['w-1'] })]);
    const v = comparisonVerdict(c as Comparison);
    expect(v).toEqual({ ok: false, text: '1 not ready (w-1)' });
  });

  test('a row recorded before not_ready was counted still reads its finding', () => {
    // Every cluster row before ea5e0e8b carries the finding and no
    // count; the finding is the record, so the absent count is not a 0.
    const [c] = parseComparisons([clusterRow({}, { not_ready: ['cp-2', 'w-1'] })]);
    expect(comparisonVerdict(c as Comparison)).toEqual({ ok: false, text: '2 not ready (cp-2, w-1)' });
  });

  test('a host that is not ready says so on its own line', () => {
    const [c] = parseComparisons([{ payload: {
      scope: 'host', observed_at: '2026-09-27T12:00:00Z', host: 'forge',
      counts: { observed: 1, observed_not_declared: 0, drift: 0, disk_tight: 0, not_ready: 1 },
      findings: { not_ready: ['forge'] },
    } }]);
    expect(comparisonVerdict(c as Comparison)).toEqual({ ok: false, text: '1 not ready (forge)' });
  });

  test('unrecorded dead letters at the dispatcher are not "no drift"', () => {
    const [c] = parseComparisons([clusterRow({ dead_letters_unrecorded: 1 }, {
      dead_letters_unrecorded: [{ id: 'boss-dispatcher', dead_letters_unrecorded: 2, age_s: 600 }],
    })]);
    expect(comparisonVerdict(c as Comparison)).toEqual({
      ok: false, text: '1 dispatcher with unrecorded dead letters',
    });
  });

  test('a dispatcher whose counters went unread says why, never "no drift"', () => {
    // dead_letter_finding (estate_compare.rs) answers UNREAD, not zero,
    // when the counters could not be read: the quiet answer is the one
    // the estate alarm refuses, so the page refuses it too.
    const [c] = parseComparisons([clusterRow({}, { dispatcher_unread: 'curl: (7) Failed to connect' })]);
    expect(comparisonVerdict(c as Comparison)).toEqual({
      ok: false, text: 'dispatcher dead-letter counters unread: curl: (7) Failed to connect',
    });
  });

  test('all three clear is still the good state', () => {
    const [c] = parseComparisons([clusterRow({}, {})]);
    expect(comparisonVerdict(c as Comparison)).toEqual({ ok: true, text: '5 observed, 5 declared — no drift' });
  });
});

// THE HOST COMPARISON (backlog 2d8d983b; page audit 2cff1d6e, GAP 1).
// The page rendered the cluster's verdict only, so every host row's
// drift (forge memory declared 30, observed 31) and boss-gcp's
// disk_tight (13 G free against a 17 G floor) never reached it. A host
// comparison is self-scoped — compare_host in estate_compare.rs stamps
// `host` and counts observed / observed_not_declared / drift /
// disk_tight, with no declared total.

/** A host comparison as the reader serves it, shaped by compare_host. */
function hostCmp(host: string | null, observed_at: string, counts: Record<string, number> = {}): unknown {
  return {
    payload: {
      scope: 'host', observed_at, host,
      counts: { observed: 1, observed_not_declared: 0, drift: 0, disk_tight: 0, ...counts },
    },
  };
}

describe('the host comparison', () => {
  test('the parser keeps the host a self-scoped comparison names; a cluster row names none', () => {
    const rows = parseComparisons([
      hostCmp('forge', '2026-09-23T16:15:00Z', { drift: 1 }),
      { payload: { scope: 'kubernetes-nodes', observed_at: '2026-09-23T16:16:00Z', counts: { observed: 5 } } },
    ]);
    expect(rows.map((r) => r.host)).toEqual(['forge', null]);
  });

  test('the newest row per host wins, one line per host, in host order', () => {
    // Newest first, as the reader serves it: forge's older drift-free
    // row is hidden by its newer one, and boss-gcp's daily row — older
    // than both — still has a line of its own (the per-host collapse
    // 3d1678ba names for the observation series).
    const latest = latestPerHost(parseComparisons([
      hostCmp('forge', '2026-09-23T16:15:00Z', { drift: 1 }),
      hostCmp('forge', '2026-09-23T16:00:00Z'),
      hostCmp('boss-gcp', '2026-09-23T10:25:00Z', { drift: 1, disk_tight: 1 }),
    ]));
    expect(latest.map((c) => [c.host, c.observed_at])).toEqual([
      ['boss-gcp', '2026-09-23T10:25:00Z'],
      ['forge', '2026-09-23T16:15:00Z'],
    ]);
  });

  test('a host short of disk AND drifted names both, and is not clean', () => {
    const [c] = parseComparisons([hostCmp('boss-gcp', '2026-09-23T10:25:00Z', { drift: 1, disk_tight: 1 })]);
    const v = comparisonVerdict(c as Comparison);
    expect(v.ok).toBe(false);
    expect(v.text).toBe('1 drifted from declaration; 1 short of disk');
  });

  test('a clean host reads its observed count only — it carries no declared total to print as 0', () => {
    const [c] = parseComparisons([hostCmp('forge', '2026-09-23T16:15:00Z')]);
    expect(comparisonVerdict(c as Comparison)).toEqual({ ok: true, text: '1 observed — no drift' });
  });

  test('a host nobody declared is named as undeclared, not as "in the cluster"', () => {
    const [c] = parseComparisons([hostCmp('mystery-box', '2026-09-23T16:15:00Z', { observed_not_declared: 1 })]);
    const v = comparisonVerdict(c as Comparison);
    expect(v.ok).toBe(false);
    expect(v.text).toBe('1 observed but not declared');
  });

  test('the grouped read keeps host rows and its total, which counts hosts', () => {
    const page = parseHostComparisons({
      data: [
        hostCmp('forge', '2026-09-23T16:15:00Z'),
        // A server that ignored ?scope= would hand back other series;
        // they are not host comparisons, whatever the page asked for.
        { payload: { scope: 'host-units', observed_at: '2026-09-23T16:14:00Z', host: 'forge', counts: {} } },
        hostCmp('boss-gcp', '2026-09-23T10:25:00Z'),
      ],
      total: 2,
    });
    expect(page.rows.map((r) => r.host)).toEqual(['forge', 'boss-gcp']);
    expect(page.total).toBe(2);
    expect(hostPageIsWhole(page)).toBe(true);
  });

  // Backlog 725532ab: the page used to take a scoped page of 50 rows
  // and say how far back it reached, because boss-gcp's daily row fell
  // off it about half of every day. The read now groups per host on
  // the server, and the page owes a line to every DECLARED host.
  test('every declared host outside the cluster gets a line — one with no comparison says so', () => {
    const nodes = {
      kind: 'ready' as const,
      data: parseNodes([
        node({ id: 'forge', role: 'forge' }),
        node({ id: 'boss-gcp', role: 'bastion' }),
        node({ id: 'lab-1', role: 'lab-host' }),
        node({ id: 'w-1', role: 'talos-worker' }),
        node({ id: 'old-host', role: 'forge', retired: true }),
      ]),
    };
    const page = parseHostComparisons({
      data: [hostCmp('forge', '2026-09-25T07:45:00Z', { drift: 1 }), hostCmp('boss-gcp', '2026-09-25T10:25:00Z')],
      total: 2,
    });
    const lines = hostLines(nodes, page);
    expect(lines.map((l) => [l.host, l.cmp?.observed_at ?? null])).toEqual([
      ['boss-gcp', '2026-09-25T10:25:00Z'],
      ['forge', '2026-09-25T07:45:00Z'],
      // Declared, outside the cluster, live — and no comparison: a line.
      ['lab-1', null],
    ]);
    expect(missingHostText(page)).toBe('no host comparison recorded');
  });

  test('a host that compared without being declared keeps its line; an unread registry leaves only the rows', () => {
    const page = parseHostComparisons({ data: [hostCmp('mystery-box', '2026-09-25T10:00:00Z')], total: 1 });
    const failed = { kind: 'failed' as const, error: 'HTTP 503' };
    expect(hostLines(failed, page).map((l) => l.host)).toEqual(['mystery-box']);
    const declared = { kind: 'ready' as const, data: parseNodes([node({ id: 'forge', role: 'forge' })]) };
    expect(hostLines(declared, page).map((l) => l.host)).toEqual(['forge', 'mystery-box']);
  });

  test('a read that is not whole never tells a declared host it has no comparison', () => {
    // More hosts than the page held (rows < total), or a server that
    // did not count: the absence is of the READ, not of the host.
    const truncated = parseHostComparisons({ data: [hostCmp('forge', '2026-09-25T07:45:00Z')], total: 3 });
    expect(hostPageIsWhole(truncated)).toBe(false);
    expect(missingHostText(truncated)).toBe('not among the 1 of 3 hosts this read returned');
    expect(hostCoverText(truncated)).toBe('The host read returned 1 of 3 hosts: a host past it has no comparison shown here.');
    const uncounted = parseHostComparisons([hostCmp('forge', '2026-09-25T07:45:00Z')]);
    expect(hostPageIsWhole(uncounted)).toBe(false);
    // An older reader that ignored latest_per answers a scoped page of
    // one host's rows with the SERIES total: 1 host held, not whole.
    const ungrouped = parseHostComparisons({
      data: [hostCmp('forge', '2026-09-25T07:45:00Z'), hostCmp('forge', '2026-09-25T07:30:00Z')],
      total: 768,
    });
    expect(hostCoverText(ungrouped)).toBe('The host read returned 1 of 768 hosts: a host past it has no comparison shown here.');
    // Nothing at all is a whole answer, counted or not.
    expect(hostPageIsWhole(parseHostComparisons([]))).toBe(true);
    expect(hostCoverText(parseHostComparisons({ data: [hostCmp('forge', '2026-09-25T07:45:00Z')], total: 1 }))).toBeNull();
  });

  test('fetchEstate reads the host comparisons grouped per host, in a read of their own', async () => {
    const asked: string[] = [];
    globalThis.fetch = (async (url: RequestInfo | URL) => {
      const u = String(url);
      asked.push(u);
      const body = u === HOST_COMPARISONS_READ
        ? { data: [hostCmp('forge', '2026-09-23T16:15:00Z', { drift: 1 })], total: 1 }
        : u.includes('/nodes') ? [node()] : [];
      return new Response(JSON.stringify(body), { status: 200 });
    }) as unknown as typeof fetch;
    const s = await fetchEstate();
    expect(HOST_COMPARISONS_READ).toBe('/api/estate/comparisons?scope=host&latest_per=host&limit=50');
    expect(asked).toContain(HOST_COMPARISONS_READ);
    expect(s.hostComparisons.kind).toBe('ready');
    if (s.hostComparisons.kind === 'ready') {
      expect(s.hostComparisons.data.rows.map((r) => [r.host, r.counts.drift])).toEqual([['forge', 1]]);
    }
  });
});

describe('fetchEstate', () => {
  test('an unreachable registry lands as failed, never as an empty estate', async () => {
    globalThis.fetch = (async () => {
      throw new Error('connect ECONNREFUSED');
    }) as unknown as typeof fetch;
    const s = await fetchEstate();
    expect(s.nodes.kind).toBe('failed');
    expect(s.cluster.kind).toBe('failed');
    expect(s.comparisons.kind).toBe('failed');
    expect(s.hostComparisons.kind).toBe('failed');
    expect(s.alarms.kind).toBe('failed');
    expect(s.volumes.kind).toBe('failed');
    // With neither host source answering, no host series was planned —
    // and the state says it does not know, rather than "no hosts".
    expect(s.hosts).toEqual({ known: false, series: [] });
  });

  test('good reads land ready with parsed rows', async () => {
    globalThis.fetch = (async (url: RequestInfo | URL) => {
      const u = String(url);
      const body = u.includes('/nodes')
        ? [node()]
        : u.includes('/observations')
          ? { data: [obsEvent('kubernetes-nodes', '2026-08-31T10:25:00Z')], total: 1 }
          : [];
      return new Response(JSON.stringify(body), { status: 200 });
    }) as unknown as typeof fetch;
    const s = await fetchEstate();
    expect(s.nodes.kind).toBe('ready');
    if (s.nodes.kind === 'ready') expect(s.nodes.data[0]?.id).toBe('w-1');
    expect(s.cluster.kind).toBe('ready');
  });

  // The pin the finding asked for: a slow series that falls off an
  // unscoped page. The unscoped page here is the live one of 2026-09-27
  // in miniature — spent by the five-minute series — and boss-gcp's
  // daily host reading is nowhere on it. The page must never ask it.
  test('a daily host reading that falls off an unscoped page is still read, from its own series', async () => {
    const asked: string[] = [];
    const unscoped = Array.from({ length: 20 }, (_, i) =>
      obsEvent('host-units', `2026-09-27T17:${String(14 - (i % 10)).padStart(2, '0')}:00Z`, [{ id: i % 2 ? 'forge' : 'boss-gcp', units: [] }]));
    const series: Record<string, unknown[]> = {
      'kubernetes-nodes:': [obsEvent('kubernetes-nodes', '2026-09-27T17:00:02Z', [{ id: 'cp-1' }])],
      'host:forge': [obsEvent('host', '2026-09-27T17:05:45Z', [{ id: 'forge', disk_free_gb: 210 }])],
      'host:boss-gcp': [obsEvent('host', '2026-09-27T10:25:01Z', [{ id: 'boss-gcp', disk_free_gb: 13 }])],
      'host-units:forge': [unscoped[1]!],
      'host-units:boss-gcp': [unscoped[0]!],
    };
    globalThis.fetch = (async (url: RequestInfo | URL) => {
      const u = new URL(String(url), 'http://page.test');
      asked.push(`${u.pathname}${u.search}`);
      if (u.pathname === '/api/estate/nodes') {
        return new Response(JSON.stringify([node({ id: 'forge', role: 'forge' }), node({ id: 'boss-gcp', role: 'bastion' })]), { status: 200 });
      }
      if (u.pathname === '/api/estate/observations') {
        const scope = u.searchParams.get('scope');
        if (scope === null) return new Response(JSON.stringify({ data: unscoped, total: 6106 }), { status: 200 });
        const rows = series[`${scope}:${u.searchParams.get('host') ?? ''}`] ?? [];
        return new Response(JSON.stringify({ data: rows, total: rows.length }), { status: 200 });
      }
      return new Response(JSON.stringify({ data: [], total: 0 }), { status: 200 });
    }) as unknown as typeof fetch;

    const s = await fetchEstate();
    expect(asked.filter((a) => a.startsWith('/api/estate/observations') && !a.includes('scope='))).toEqual([]);
    expect(s.hosts.known).toBe(true);
    const gcp = s.hosts.series.find((h) => h.host === 'boss-gcp');
    expect(gcp?.readings.kind).toBe('ready');
    if (gcp?.readings.kind === 'ready') {
      expect(gcp.readings.data.rows.map((r) => [r.observed_at, r.nodes[0]?.disk_free_gb])).toEqual([['2026-09-27T10:25:01Z', 13]]);
    }
    expect(s.hosts.series.map((h) => h.host)).toEqual(['boss-gcp', 'forge']);
    expect(s.hosts.series.every((h) => h.units.kind === 'ready')).toBe(true);
  });

  test('a host series whose read failed lands failed on that host alone', async () => {
    globalThis.fetch = (async (url: RequestInfo | URL) => {
      const u = String(url);
      if (u.includes('/nodes')) return new Response(JSON.stringify([node({ id: 'forge', role: 'forge' })]), { status: 200 });
      if (u.includes('scope=host-units')) return new Response('', { status: 503 });
      return new Response(JSON.stringify({ data: [], total: 0 }), { status: 200 });
    }) as unknown as typeof fetch;
    const s = await fetchEstate();
    const [forge] = s.hosts.series;
    expect(forge?.units).toEqual({ kind: 'failed', error: '/api/estate/observations?scope=host-units&host=forge&limit=10: HTTP 503' });
    expect(forge?.readings.kind).toBe('ready');
  });
});

// THE LOOPS (backlog 0d9b2960; page audit 2cff1d6e, GAP 10). The page
// showed none of the estate's own working or finished packets, so "did
// the loop run" had no answer on it. Each loop's newest terminal is that
// answer; an open packet is the run in flight (or stuck).

/** A packet as GET /api/jobs lists it — only the keys the page reads. */
function jobRow(over: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    id: 'aaaa1111-0000-0000-0000-000000000000',
    kind: 'maintenance-cluster-watchdog',
    status: 'closed',
    opened_at: '2026-09-24T11:14:28Z',
    metadata: { outcome: 'completed', closed_at: '2026-09-24T11:14:34Z', opened_at: '2026-09-24T11:14:28Z' },
    steps: [{ spec_slug: 'run', metadata: { result: 'ok' } }],
    ...over,
  };
}

describe('parseLoopPackets', () => {
  test('a terminal carries its outcome and the instant it closed', () => {
    const [p] = parseLoopPackets({ data: [jobRow()], total: 1 });
    expect(p).toEqual({
      id: 'aaaa1111-0000-0000-0000-000000000000',
      status: 'closed',
      outcome: 'completed',
      at: '2026-09-24T11:14:34Z',
      host: null,
      tunnel: null,
    });
  });

  test('an open packet is dated from when it opened, and has no outcome yet', () => {
    const [p] = parseLoopPackets([jobRow({ status: 'open', opened_at: '2026-09-24T11:20:00Z', metadata: {} })]);
    expect(p?.status).toBe('open');
    expect(p?.outcome).toBeNull();
    expect(p?.at).toBe('2026-09-24T11:20:00Z');
  });

  test('the host is the one the packet names: an ops-request on its metadata, a converge on its run step', () => {
    // Measured 2026-09-24: ops-request metadata.host = "forge"; the
    // forge and boss-gcp converges stamp node_id on the run step; the
    // watchdog and the observers name no host at all.
    const [ops] = parseLoopPackets([jobRow({ kind: 'ops-request', metadata: { host: 'boss-gcp', outcome: 'answered' } })]);
    expect(ops?.host).toBe('boss-gcp');
    const [conv] = parseLoopPackets([jobRow({ steps: [{ spec_slug: 'run', metadata: { node_id: 'forge', result: 'ok' } }] })]);
    expect(conv?.host).toBe('forge');
    const [none] = parseLoopPackets([jobRow()]);
    expect(none?.host).toBeNull();
  });

  test('a row with no id is refused, not rendered as a link to nowhere', () => {
    expect(() => parseLoopPackets([{ status: 'closed' }])).toThrow();
  });
});

describe('loopPlan', () => {
  const nodes = [
    parseNodes([node({ id: 'forge', role: 'forge', roles: ['cluster-operator', OPS_RUNNER_ROLE] })])[0]!,
    parseNodes([node({ id: 'boss-gcp', role: 'bastion', roles: [OPS_RUNNER_ROLE] })])[0]!,
    parseNodes([node({ id: 'old-box', role: 'forge', roles: [OPS_RUNNER_ROLE], retired: true })])[0]!,
    parseNodes([node({ id: 'w-1' })])[0]!,
  ];

  test('every declared loop, then one ops-request row per live host that declares the runner role', () => {
    const plan = loopPlan({ kind: 'ready', data: nodes });
    expect(plan.map((p) => [p.kind, p.host])).toEqual([
      ...ESTATE_LOOPS.map((l) => [l.kind, null]),
      [OPS_REQUEST_KIND, 'forge'],
      [OPS_REQUEST_KIND, 'boss-gcp'],
    ]);
  });

  test('an unreadable registry still asks whether ANY runner answered, rather than dropping the row', () => {
    const plan = loopPlan({ kind: 'failed', error: 'down' });
    expect(plan.filter((p) => p.kind === OPS_REQUEST_KIND)).toEqual([
      { kind: OPS_REQUEST_KIND, label: 'ops-request', host: null },
    ]);
  });
});

describe('loopQueries', () => {
  test('the newest terminal is one closed row; the open read is every open packet of the kind', () => {
    expect(loopQueries('maintenance-cluster-watchdog', null)).toEqual({
      latest: '/api/jobs?kind=maintenance-cluster-watchdog&status=closed&limit=1&full=true',
      open: '/api/jobs?kind=maintenance-cluster-watchdog&status=open&full=true',
    });
  });

  test('a host row is narrowed by metadata containment, the filter the server applies', () => {
    const q = loopQueries(OPS_REQUEST_KIND, 'forge');
    const filter = `&metadata=${encodeURIComponent(JSON.stringify({ host: 'forge' }))}`;
    expect(q.latest).toBe(`/api/jobs?kind=ops-request&status=closed&limit=1&full=true${filter}`);
    expect(q.open).toBe(`/api/jobs?kind=ops-request&status=open&full=true${filter}`);
  });
});

describe('loopHost', () => {
  const row = (over: Partial<LoopRow>): LoopRow => ({
    kind: 'k', label: 'k', host: null,
    latest: { kind: 'ready', data: null }, open: { kind: 'ready', data: [] },
    ...over,
  });

  test('a host row is its host; otherwise the host the newest packet names; otherwise it says so', () => {
    expect(loopHost(row({ host: 'forge' }))).toBe('forge');
    const packet = { id: 'x', status: 'closed', outcome: 'completed', at: null, host: 'boss-gcp', tunnel: null };
    expect(loopHost(row({ latest: { kind: 'ready', data: packet } }))).toBe('boss-gcp');
    expect(loopHost(row({}))).toBe('not named on the packet');
  });
});

describe('loopAge', () => {
  // A five-minute loop dated "today" answers nothing; the age is read
  // to the minute (the board's sinceText), and a missing stamp says so.
  const now = new Date('2026-09-24T12:00:00Z');
  test('minutes, then hours, from the instant the packet names', () => {
    expect(loopAge('2026-09-24T11:55:00Z', now)).toBe('5m ago');
    expect(loopAge('2026-09-24T09:00:00Z', now)).toBe('3h ago');
  });
  test('no stamp is undated, never a zero age', () => {
    expect(loopAge(null, now)).toBe('undated');
  });
});

describe('fetchEstate reads the loops', () => {
  test('an unreachable jobs API lands every loop row as failed — never as a loop that did not run', async () => {
    globalThis.fetch = (async (url: RequestInfo | URL) => {
      const u = String(url);
      if (u.startsWith('/api/jobs')) return new Response('', { status: 502 });
      const body = u.includes('/nodes') ? [node({ id: 'forge', roles: [OPS_RUNNER_ROLE] })] : [];
      return new Response(JSON.stringify(body), { status: 200 });
    }) as unknown as typeof fetch;
    const s = await fetchEstate();
    expect(s.loops.length).toBe(ESTATE_LOOPS.length + 1);
    expect(s.loops.every((l) => l.latest.kind === 'failed' && l.open.kind === 'failed')).toBe(true);
  });

  test('a kind with no closed packet reads ready-and-null, which the page renders as never finished', async () => {
    globalThis.fetch = (async (url: RequestInfo | URL) => {
      const u = String(url);
      if (u.includes('/nodes')) {
        return new Response(JSON.stringify([node({ id: 'forge', roles: [OPS_RUNNER_ROLE] })]), { status: 200 });
      }
      if (u.includes('kind=maintenance-cluster-watchdog&status=closed')) {
        return new Response(JSON.stringify({ data: [jobRow()], total: 1 }), { status: 200 });
      }
      return new Response(JSON.stringify({ data: [], total: 0 }), { status: 200 });
    }) as unknown as typeof fetch;
    const s = await fetchEstate();
    const wd = s.loops.find((l) => l.kind === 'maintenance-cluster-watchdog');
    expect(wd?.latest).toEqual({ kind: 'ready', data: parseLoopPackets([jobRow()])[0]! });
    const other = s.loops.find((l) => l.kind === 'maintenance-forge-converge');
    expect(other?.latest).toEqual({ kind: 'ready', data: null });
    expect(s.loops.find((l) => l.kind === OPS_REQUEST_KIND)?.host).toBe('forge');
  });
});

describe('the loops the page reads are in the registry', () => {
  // A kind renamed in the registry would otherwise render "never
  // finished" forever, a confident wrong answer (CLAUDE.md §9a: the
  // list lives here AND in infra/platform/workflows, so it is pinned).
  const workflow = (kind: string): string =>
    readFileSync(new URL(`../../../../../infra/platform/workflows/${kind}.toml`, import.meta.url), 'utf8');
  const kinds = [...ESTATE_LOOPS.map((l) => l.kind), OPS_REQUEST_KIND];

  test('every kind the page reads is a workflow in the tree', () => {
    for (const k of kinds) expect(workflow(k)).toContain(`kind = "${k}"`);
  });

  test('every terminal those workflows declare is one the page has decided the colour of', () => {
    // The ok set is the success terminals; everything else these
    // workflows can end in (failed, refused) must render as trouble. A
    // new terminal outcome lands here as a red test, not as a silent ok.
    const declared = new Set(
      kinds.flatMap((k) => [...workflow(k).matchAll(/terminal = \{ outcome = "([^"]+)" \}/g)].map((m) => m[1]!)),
    );
    expect([...declared].sort()).toEqual(['answered', 'completed', 'failed', 'nothing-to-do', 'refused']);
    expect([...LOOP_OK_OUTCOMES].sort()).toEqual(['answered', 'completed', 'nothing-to-do']);
  });
});

describe('the dev workspace door', () => {
  // The door moved from a LAN VIP behind a WireGuard bastion to one
  // public hostname behind a Cloudflare Access SSH application (design
  // 5fc71f03; backlog e4cedb46). The hostname is pinned to the tunnel
  // route that serves it by the Rust test
  // the_dev_door_is_an_access_ssh_application.rs — this file pins the
  // WORDS an operator pastes, which nothing else reads.
  test('the hostname is the declared one', () => {
    expect(DEV_DOOR_HOST).toBe('dev.algedonic.dev');
  });

  test('the block is the three-line setup, in order, each with its reason', () => {
    const steps = devDoorSteps();
    expect(steps.map((s) => s.command)).toEqual([
      'cloudflared --version',
      "grep -qsF 'Match host dev.algedonic.dev ' ~/.ssh/config || cloudflared access ssh-config --hostname dev.algedonic.dev --short-lived-cert | sed '/^Add to your/d' >> ~/.ssh/config",
      'ssh root@dev.algedonic.dev',
    ]);
    // A command pasted blind is a command nobody can judge.
    expect(steps.every((s) => s.why.length > 0 && s.what.length > 0)).toBe(true);
  });

  test('the route step appends only the stanza, and only once', () => {
    // cloudflared prints "Add to your <home>/.ssh/config:" above the
    // stanza. It is not a # comment, so appending it raw leaves a line
    // ssh refuses to parse, and the operator deleted it by hand every
    // time. A second run appended a second stanza, so the step is
    // guarded on the one it writes.
    const route = devDoorSteps()[1]?.command ?? '';
    expect(route).toContain("sed '/^Add to your/d'");
    expect(route.startsWith("grep -qsF 'Match host dev.algedonic.dev ' ~/.ssh/config || ")).toBe(true);
  });

  test('every step names the host it was given, so one constant moves them all', () => {
    const steps = devDoorSteps('dev.example.test');
    expect(steps[1]?.command).toContain('dev.example.test');
    expect(steps[2]?.command).toBe('ssh root@dev.example.test');
    // The root login is what the certificate's principal must match:
    // sshd with no AuthorizedPrincipalsFile requires cert principal ==
    // login name (infra/cluster/manifests/boss-dev.yaml).
    expect(steps[2]?.command.startsWith('ssh root@')).toBe(true);
  });
});

// DECLARED BESIDE OBSERVED (backlog ab3c54d7; page audit 2cff1d6e, GAP
// 7). The machines table showed declared values only, while every host
// comparison named a machine and a field — measured live 2026-09-28:
// forge memory_gb declared 30, observed 31; boss-gcp 15, observed 16.

/** An estate as fetchEstate lands it, every read answered and empty. */
function estateOf(over: Partial<EstateState> = {}): EstateState {
  const ready = <T,>(data: T) => ({ kind: 'ready' as const, data });
  return {
    nodes: ready([]),
    cluster: ready({ rows: [], returned: 0, total: 0 }),
    comparisons: ready([]),
    hostComparisons: ready({ rows: [], total: 0 }),
    hosts: { known: true, series: [] },
    loops: [],
    alarms: ready({ rows: [], total: 0 }),
    zones: { latest: ready([]), open: ready([]) },
    volumes: ready([]),
    ...over,
  };
}

const [forgeNode, w1Node] = parseNodes([
  node({ id: 'forge', role: 'forge', address: '192.0.2.15', cpu: 16, memory_gb: 30, disk_gb: 500 }),
  node({ id: 'w-1', role: 'talos-worker', address: '192.0.2.14', cpu: 32, memory_gb: 63, disk_gb: 929 }),
]) as [EstateNode, EstateNode];

const seriesOf = (scope: string, nodes: unknown[]) => ({
  kind: 'ready' as const,
  data: parseSeriesPage({ data: [obsEvent(scope, '2026-09-28T08:00:00Z', nodes)], total: 1 }, scope, null),
});

describe('observed beside declared, per machine', () => {
  test('a host is read from its own host series, a cluster node from the cluster series', () => {
    const s = estateOf({
      cluster: seriesOf('kubernetes-nodes', [{ id: 'w-1', cpu: 32, memory_gb: 62 }]),
      hosts: {
        known: true,
        series: [{
          host: 'forge',
          readings: seriesOf('host', [{ id: 'forge', address: '192.0.2.15', cpu: 16, memory_gb: 31, disk_gb: 480 }]),
          units: seriesOf('host-units', [{ id: 'forge', units: [] }]),
        }],
      },
    });
    expect(machineSight(s, forgeNode)).toEqual({ kind: 'seen', node: { id: 'forge', address: '192.0.2.15', cpu: 16, memory_gb: 31, disk_gb: 480 } });
    expect(machineSight(s, w1Node)).toEqual({ kind: 'seen', node: { id: 'w-1', cpu: 32, memory_gb: 62 } });
  });

  test('a machine missing from its newest reading, a series with none, and a failed read each say which', () => {
    expect(machineSight(estateOf({ cluster: seriesOf('kubernetes-nodes', [{ id: 'cp-1' }]) }), w1Node)).toEqual({ kind: 'absent' });
    expect(machineSight(estateOf(), w1Node)).toEqual({ kind: 'no-reading' });
    expect(machineSight(estateOf({ cluster: { kind: 'failed', error: 'x: HTTP 503' } }), w1Node)).toEqual({ kind: 'unread' });
    // A host whose series was never planned (neither source answered) is
    // unread, never "not seen".
    expect(machineSight(estateOf({ hosts: { known: false, series: [] } }), forgeNode)).toEqual({ kind: 'unread' });
  });

  test('the drift a comparison names is found per machine, from the cluster verdict and from the host\'s own', () => {
    const cluster = parseComparisons([{ payload: {
      scope: 'kubernetes-nodes', observed_at: '2026-09-28T08:00:00Z', counts: { drift: 1 },
      findings: { drift: [{ id: 'w-1', fields: { address: { declared: '192.0.2.14', observed: '192.0.2.40' } } }] },
    } }]);
    const hosts = parseHostComparisons({ data: [{ payload: {
      scope: 'host', host: 'forge', observed_at: '2026-09-28T08:00:00Z', counts: { observed: 1, drift: 1 },
      findings: { drift: [{ id: 'forge', fields: { memory_gb: { declared: 30, observed: 31 } } }] },
    } }], total: 1 });
    const s = estateOf({ comparisons: { kind: 'ready', data: cluster }, hostComparisons: { kind: 'ready', data: hosts } });
    expect(machineDrift(s, 'forge')).toEqual({ memory_gb: { declared: 30, observed: 31 } });
    expect(machineDrift(s, 'w-1')).toEqual({ address: { declared: '192.0.2.14', observed: '192.0.2.40' } });
    expect(machineDrift(s, 'cp-1')).toEqual({});
    // A comparison read that failed names no drift — the cells then read
    // the observation alone, and the failure line says why.
    expect(machineDrift(estateOf({ hostComparisons: { kind: 'failed', error: 'x' } }), 'forge')).toEqual({});
  });

  test('each cell reads what was seen, and a field the drift finding names says so', () => {
    const seen = { kind: 'seen', node: { id: 'forge', address: '192.0.2.15', cpu: 16, memory_gb: 31, disk_gb: 480 } } as const;
    const drift = { memory_gb: { declared: 30, observed: 31 } };
    expect(MACHINE_FIELDS.map((f) => seenCell(seen, drift, f).text)).toEqual([
      'seen 192.0.2.15', 'seen 16', 'seen 31G · drifted', 'seen 480G',
    ]);
    expect(seenCell(seen, drift, 'memory_gb').drift).toEqual({ declared: 30, observed: 31 });
    expect(seenCell(seen, drift, 'cpu').drift).toBeNull();
    // A field the reading did not carry is a dash, never a zero.
    expect(seenCell({ kind: 'seen', node: { id: 'w-1' } }, {}, 'cpu').text).toBe('seen —');
    expect(seenCell({ kind: 'absent' }, {}, 'cpu').text).toBe('not seen');
    expect(seenCell({ kind: 'no-reading' }, {}, 'cpu').text).toBe('no reading');
    expect(seenCell({ kind: 'unread' }, {}, 'cpu').text).toBe('unread');
  });

  test('a drift the finding names is marked even when the newest reading has moved on', () => {
    // The finding is the record of what was compared; the cell shows the
    // value the comparison judged, so the mark and the number agree.
    const cell = seenCell({ kind: 'seen', node: { id: 'forge', memory_gb: 30 } }, { memory_gb: { declared: 30, observed: 31 } }, 'memory_gb');
    expect(cell.text).toBe('seen 31G · drifted');
  });

  test('the declared value reads as the table always read it', () => {
    expect(MACHINE_FIELDS.map((f) => machineValue(f, forgeNode[f]))).toEqual(['192.0.2.15', '16', '30G', '500G']);
    expect(machineValue('cpu', null)).toBe('—');
  });

  test('the drift finding names the machine and the field in the verdict, not only a count', () => {
    const [c] = parseComparisons([{ payload: {
      scope: 'host', host: 'forge', observed_at: '2026-09-28T08:00:00Z', counts: { observed: 1, drift: 1 },
      findings: { drift: [{ id: 'forge', fields: { memory_gb: { declared: 30, observed: 31 } } }] },
    } }]);
    expect(comparisonVerdict(c as Comparison).text).toBe('1 drifted from declaration (forge memory_gb 30 → 31)');
  });
});

// OPEN ESTATE ALARMS (backlog 48ef9961; page audit 2cff1d6e, GAP 9). The
// alarms that explain an amber line were neither shown nor linked here.

function alarmRow(over: Record<string, unknown> = {}, md: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    id: 'd3c7eada-0000-0000-0000-000000000000',
    kind: 'backlog-item',
    status: 'open',
    title: 'ESTATE ALARM: disk_tight:boss-gcp persisted 3 consecutive comparisons',
    opened_at: '2026-09-26T10:25:02Z',
    metadata: { area: 'estate', estate_finding: 'disk_tight:boss-gcp', scope: 'host', host: 'boss-gcp', ...md },
    ...over,
  };
}

describe('the open estate alarms', () => {
  test('are read as the raiser keys them: open backlog-items carrying estate_finding', () => {
    expect(OPEN_ALARMS_READ).toBe('/api/jobs?kind=backlog-item&status=open&metadata_has=estate_finding&limit=50');
    // The key lives twice — the raiser writes it and dedups on it, this
    // page reads it (CLAUDE.md §9a) — so it is pinned to the raiser.
    const raiser = readFileSync(
      new URL('../../../../../crates/orchestrators/boss-dispatcher-handlers/src/handlers/estate_alarm.rs', import.meta.url),
      'utf8',
    );
    expect(raiser).toContain('"estate_finding": key');
    expect(raiser).toContain('metadata_has=estate_finding');
  });

  test('each alarm carries its finding, its scope and host, and when it opened; the page keeps the total', () => {
    const page = parseAlarms({ data: [alarmRow(), alarmRow({ id: 'e', title: 'ESTATE ALARM: dev-ssh…' }, { scope: 'door', host: undefined, estate_finding: 'door:dev-ssh' })], total: 2 });
    expect(page.total).toBe(2);
    expect(page.rows).toEqual([
      { id: 'd3c7eada-0000-0000-0000-000000000000', title: 'ESTATE ALARM: disk_tight:boss-gcp persisted 3 consecutive comparisons', finding: 'disk_tight:boss-gcp', scope: 'host', host: 'boss-gcp', at: '2026-09-26T10:25:02Z' },
      { id: 'e', title: 'ESTATE ALARM: dev-ssh…', finding: 'door:dev-ssh', scope: 'door', host: null, at: '2026-09-26T10:25:02Z' },
    ]);
    // A bare array (an older reader) keeps its rows and says it did not count.
    expect(parseAlarms([alarmRow()]).total).toBeNull();
  });

  test('a row with no id is refused, not rendered as a link to nowhere', () => {
    expect(() => parseAlarms({ data: [alarmRow({ id: undefined })], total: 1 })).toThrow();
  });

  test('the alarms beside a verdict are the ones on its series: same scope, same host', () => {
    const alarms = { kind: 'ready' as const, data: parseAlarms({ data: [
      alarmRow(),
      alarmRow({ id: 'u' }, { scope: 'host-units', estate_finding: 'unit_unhealthy:boss-gcp/x.service' }),
      alarmRow({ id: 'k' }, { scope: 'kubernetes-nodes', host: undefined, estate_finding: 'unobserved:kubernetes-nodes' }),
    ], total: 3 }) };
    expect(alarmsOn(alarms, 'host', 'boss-gcp').map((a) => a.id)).toEqual(['d3c7eada-0000-0000-0000-000000000000']);
    expect(alarmsOn(alarms, 'host', 'forge')).toEqual([]);
    expect(alarmsOn(alarms, 'host-units', 'boss-gcp').map((a) => a.id)).toEqual(['u']);
    expect(alarmsOn(alarms, 'kubernetes-nodes', null).map((a) => a.id)).toEqual(['k']);
    expect(alarmsOn({ kind: 'failed', error: 'x' }, 'host', 'boss-gcp')).toEqual([]);
  });

  test('a read that holds fewer alarms than it counted says so; a whole one says nothing', () => {
    expect(alarmCoverText({ rows: [], total: 0 })).toBeNull();
    const one = parseAlarms({ data: [alarmRow()], total: 1 });
    expect(alarmCoverText(one)).toBeNull();
    expect(alarmCoverText({ ...one, total: 60 })).toBe('The alarm read returned 1 of 60 open estate alarms: the rest are not listed here.');
    expect(alarmCoverText({ ...one, total: null })).toBeNull();
  });

  test('fetchEstate reads them once, and a failed read lands failed — never as no alarm', async () => {
    const asked: string[] = [];
    globalThis.fetch = (async (url: RequestInfo | URL) => {
      const u = String(url);
      asked.push(u);
      if (u.includes('kind=backlog-item')) return new Response('', { status: 503 });
      return new Response(JSON.stringify({ data: [], total: 0 }), { status: 200 });
    }) as unknown as typeof fetch;
    const s = await fetchEstate();
    expect(asked.filter((u) => u.includes('kind=backlog-item'))).toEqual([OPEN_ALARMS_READ]);
    expect(s.alarms).toEqual({ kind: 'failed', error: `${OPEN_ALARMS_READ}: HTTP 503` });
  });
});

// THE EDGE (backlog e0e183fb; page audit 2cff1d6e, GAP 11). The DNS
// zone, the Access applications in front of it and the tunnel routes
// behind it are declared in the tree and read back daily — the zone and
// Access on a dns-zone-observation packet, the routes on the cluster
// converge's run step — and the page rendered none of it, so the one
// estate alarm about the zone had no line to stand beside. The shapes
// below are the live ones measured 2026-10-01 (packet d6ecf14f and
// converge 05ec4e89), with the names moved to example.org.

const TUNNEL = 'd8a8ef3b-0000-0000-0000-000000000000.cfargotunnel.com';

/** One dns-zone-observation packet as GET /api/jobs?full=true lists it. */
function zoneRow(over: Record<string, unknown> = {}, observe: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    id: 'd6ecf14f-0000-0000-0000-000000000000',
    kind: 'dns-zone-observation',
    status: 'closed',
    subject: { id: 'example.org', subject_kind: 'custom' },
    opened_at: '2026-10-01T00:00:10Z',
    metadata: { zone: 'example.org', outcome: 'matched', closed_at: '2026-10-01T00:00:15Z' },
    steps: [
      { spec_slug: 'due', metadata: {} },
      {
        spec_slug: 'observe',
        metadata: {
          result: 'match',
          verdicts: [
            {
              record: 'boss.example.org CNAME', name: 'boss.example.org', type: 'CNAME', verdict: 'MATCH',
              declared: { content: TUNNEL, proxied: true, ttl: 1, target: 'tunnel:cloudflare-tunnel-credentials' },
              live: { content: TUNNEL, proxied: true, ttl: 1 },
              interlock: 'access', access: 'present', why: 'the operating site',
            },
            {
              record: 'id.example.org CNAME', name: 'id.example.org', type: 'CNAME', verdict: 'MATCH',
              declared: { content: TUNNEL, proxied: true, ttl: 1, target: 'tunnel:cloudflare-tunnel-credentials' },
              live: { content: TUNNEL, proxied: true, ttl: 1 },
              interlock: 'tunnel', tunnel: 'routed', why: 'the identity provider',
            },
            { record: 'example.org MX', name: 'example.org', type: 'MX', verdict: 'UNDECLARED', live: { content: 'mail.example.net' } },
            { record: 'example.org TXT', name: 'example.org', type: 'TXT', verdict: 'UNDECLARED', live: { content: '"v=spf1 ~all"' } },
          ],
          access: [
            {
              application: 'boss.example.org', domain: 'boss.example.org', verdict: 'MATCH', why: 'the operating site behind Access',
              declared: { name: 'BOSS', type: 'self_hosted', session_duration: '24h', policies: [{ name: 'operators', decision: 'allow', emails: ['op@example.org'] }] },
              live: { id: 'x', name: 'BOSS', type: 'self_hosted', policies: [{ name: 'operators', decision: 'allow', include: [{ email: { email: 'op@example.org' } }] }] },
            },
          ],
          ...observe,
        },
      },
    ],
    ...over,
  };
}

describe('the edge: the zone, the Access applications, the tunnel routes', () => {
  test('are read off the packets the daily zone observation leaves — whole steps, newest first, and the open ones', () => {
    expect(ZONE_READINGS_READ).toBe('/api/jobs?kind=dns-zone-observation&status=closed&limit=7&full=true');
    expect(ZONE_OPEN_READ).toBe('/api/jobs?kind=dns-zone-observation&status=open');
  });

  test('each declared record carries the tree\'s spelling, what the zone holds, what fronts it, and its verdict', () => {
    const [z] = parseZoneReadings({ data: [zoneRow()], total: 1 });
    expect(z?.zone).toBe('example.org');
    expect(z?.id).toBe('d6ecf14f-0000-0000-0000-000000000000');
    expect(z?.at).toBe('2026-10-01T00:00:15Z');
    expect(z?.records).toEqual([
      { record: 'boss.example.org CNAME', verdict: 'MATCH', declared: 'tunnel:cloudflare-tunnel-credentials', live: TUNNEL, front: 'access: present', note: null, why: 'the operating site' },
      { record: 'id.example.org CNAME', verdict: 'MATCH', declared: 'tunnel:cloudflare-tunnel-credentials', live: TUNNEL, front: 'tunnel: routed', note: null, why: 'the identity provider' },
    ]);
    // Live records the tree names nowhere are reported, never listed as
    // declared: a count.
    expect(z?.undeclared).toBe(2);
  });

  test('each Access application carries its type and its policies by name and decision — never the people they admit', () => {
    const [z] = parseZoneReadings([zoneRow()]);
    expect(z?.access).toEqual([
      { domain: 'boss.example.org', type: 'self_hosted', policies: ['operators (allow)'], verdict: 'MATCH', note: null, why: 'the operating site behind Access' },
    ]);
    expect(JSON.stringify(z?.access)).not.toContain('op@example.org');
    expect(z?.accessUndeclared).toBe(0);
  });

  test('a drifted record keeps both values, a held one its reason, a declared-but-absent one no live value', () => {
    const [z] = parseZoneReadings([zoneRow({}, {
      result: 'findings',
      verdicts: [
        { record: 'www.example.org CNAME', verdict: 'DRIFT', declared: { content: TUNNEL }, live: { content: 'old.example.net' } },
        { record: 'dev.example.org CNAME', verdict: 'HELD', declared: { content: TUNNEL }, held: 'flip held — Access app absent', interlock: 'access', access: 'absent' },
        { record: 'gone.example.org A', verdict: 'ABSENT', declared: { content: '192.0.2.7' } },
      ],
      access: [{ domain: 'new.example.org', verdict: 'REFUSED', write: 'create', error: '12130 policy precedences must be unique' }],
    })]);
    expect(z?.records.map((r) => [r.verdict, r.declared, r.live, r.note])).toEqual([
      ['DRIFT', TUNNEL, 'old.example.net', null],
      ['HELD', TUNNEL, null, 'flip held — Access app absent'],
      ['ABSENT', '192.0.2.7', null, null],
    ]);
    expect(z?.access.map((a) => [a.domain, a.verdict, a.note])).toEqual([
      ['new.example.org', 'REFUSED', 'create: 12130 policy precedences must be unique'],
    ]);
  });

  test('the newest reading per zone wins; an older reading of the same zone is not drawn twice', () => {
    const zones = parseZoneReadings({ data: [
      zoneRow(),
      zoneRow({ id: 'older', metadata: { zone: 'example.org', closed_at: '2026-09-30T00:00:15Z' } }),
      zoneRow({ id: 'other', subject: { id: 'example.net' }, metadata: { closed_at: '2026-09-30T00:00:15Z' } }),
    ], total: 3 });
    expect(zones.map((z) => [z.zone, z.id])).toEqual([
      ['example.org', 'd6ecf14f-0000-0000-0000-000000000000'],
      // The zone falls back to the packet's subject, which is the zone.
      ['example.net', 'other'],
    ]);
  });

  test('a row with no id is refused, not rendered as a link to nowhere', () => {
    expect(() => parseZoneReadings([zoneRow({ id: undefined })])).toThrow();
  });

  test('the verdict counts what matches of what is declared, and only drift, absence and refusal are trouble', () => {
    const [ok] = parseZoneReadings([zoneRow()]);
    expect(zoneVerdict(ok!)).toEqual({ ok: true, text: '2 of 2 declared records match · 1 of 1 Access applications match' });
    const [held] = parseZoneReadings([zoneRow({}, {
      verdicts: [{ record: 'dev.example.org CNAME', verdict: 'HELD', declared: { content: TUNNEL }, held: 'flip held' }],
      access: [],
    })]);
    // HELD is a designed wait, not a finding (dns_observe.rs).
    expect(zoneVerdict(held!)).toEqual({ ok: true, text: '0 of 1 declared records match, 1 held · 0 of 0 Access applications match' });
    const [bad] = parseZoneReadings([zoneRow({}, {
      verdicts: [{ record: 'www.example.org CNAME', verdict: 'DRIFT', declared: { content: TUNNEL }, live: { content: 'x' } }],
    })]);
    expect(zoneVerdict(bad!).ok).toBe(false);
  });

  test('an open reading is a zone that could not be read or compared, dated from when it opened', () => {
    expect(parseZoneOpen({ data: [zoneRow({ id: 'o', status: 'open', opened_at: '2026-10-01T00:00:10Z', metadata: { zone: 'example.org' } })], total: 1 }))
      .toEqual([{ id: 'o', zone: 'example.org', at: '2026-10-01T00:00:10Z' }]);
  });

  test('the zone\'s alarm is the one its raiser keys dns_drift:<zone>, and lands beside it', () => {
    const alarms = { kind: 'ready' as const, data: parseAlarms({ data: [
      alarmRow(),
      alarmRow({ id: 'z' }, { scope: 'dns-zone', host: undefined, estate_finding: 'dns_drift:example.org', zone: 'example.org' }),
      alarmRow({ id: 'n' }, { scope: 'dns-zone', host: undefined, estate_finding: 'dns_drift:example.net', zone: 'example.net' }),
    ], total: 3 }) };
    expect(zoneAlarms(alarms, 'example.org').map((a) => a.id)).toEqual(['z']);
    expect(zoneAlarms({ kind: 'failed', error: 'x' }, 'example.org')).toEqual([]);
    // The key lives twice — the raiser writes it, this page reads it —
    // so it is pinned to the raiser (CLAUDE.md §9a).
    const raiser = readFileSync(
      new URL('../../../../../crates/orchestrators/boss-dispatcher-handlers/src/handlers/dns_observe.rs', import.meta.url),
      'utf8',
    );
    expect(raiser).toContain('format!("dns_drift:{zone}")');
  });

  test('the kind and the step keys the page reads are the ones the tree declares and records', () => {
    const workflow = readFileSync(new URL(`../../../../../infra/platform/workflows/${ZONE_KIND}.toml`, import.meta.url), 'utf8');
    expect(workflow).toContain(`kind = "${ZONE_KIND}"`);
    expect(workflow).toContain('title = "observe"');
    const handler = readFileSync(
      new URL('../../../../../crates/orchestrators/boss-dispatcher-handlers/src/handlers/dns_observe.rs', import.meta.url),
      'utf8',
    );
    expect(handler).toContain('metadata.insert("verdicts".into()');
    expect(handler).toContain('metadata.insert("access".into()');
    const converge = readFileSync(new URL('../../../../../infra/forge/cluster-deploy-lib.sh', import.meta.url), 'utf8');
    expect(converge).toContain('run_summary_field tunnel_ingress');
    expect(converge).toContain('run_summary_field cloudflared');
    expect(ESTATE_LOOPS.map((l) => l.kind)).toContain(CLUSTER_CONVERGE_KIND);
  });

  test('a converge\'s run step carries the tunnel routes it applied and the connector\'s state', () => {
    const [p] = parseLoopPackets([jobRow({ kind: CLUSTER_CONVERGE_KIND, steps: [{ spec_slug: 'run', metadata: {
      result: 'ok', cloudflared: 'connected',
      tunnel_ingress: 'boss.example.org → boss; www.example.org → boss (site); dev.example.org → ssh://boss-dev-ssh:22 (origin)',
    } }] })]);
    expect(p?.tunnel).toEqual({
      connector: 'connected',
      routes: ['boss.example.org → boss', 'www.example.org → boss (site)', 'dev.example.org → ssh://boss-dev-ssh:22 (origin)'],
    });
    // Every other loop records none.
    expect(parseLoopPackets([jobRow()])[0]?.tunnel).toBeNull();
  });

  test('the tunnel line reads the newest cluster converge, and says which of unread, never run or unrecorded it is', () => {
    const conv = (latest: LoopRow['latest']): LoopRow[] => [{
      kind: CLUSTER_CONVERGE_KIND, label: 'cluster converge', host: null, latest, open: { kind: 'ready', data: [] },
    }];
    const packet = (tunnel: { connector: string | null; routes: string[] } | null) =>
      ({ id: 'c', status: 'closed', outcome: 'completed', at: '2026-10-01T11:21:48Z', host: 'forge', tunnel });
    expect(tunnelLine(conv({ kind: 'ready', data: packet({ connector: 'connected', routes: ['a → b', 'c → d'] }) }))).toEqual({
      ok: true, text: 'connector connected · 2 routes', id: 'c', at: '2026-10-01T11:21:48Z', routes: ['a → b', 'c → d'],
    });
    expect(tunnelLine(conv({ kind: 'ready', data: packet({ connector: 'disconnected', routes: ['a → b'] }) })).ok).toBe(false);
    expect(tunnelLine(conv({ kind: 'ready', data: packet(null) })).text).toBe('the newest cluster converge recorded no tunnel routes');
    expect(tunnelLine(conv({ kind: 'ready', data: null })).text).toBe('no cluster converge has finished, so no route is recorded');
    expect(tunnelLine(conv({ kind: 'failed', error: 'x: HTTP 503' }))).toMatchObject({ ok: false, text: 'unread: x: HTTP 503' });
  });

  test('fetchEstate reads the zone readings and the open ones, and a failed read lands failed — never as no zone', async () => {
    const asked: string[] = [];
    globalThis.fetch = (async (url: RequestInfo | URL) => {
      const u = String(url);
      asked.push(u);
      if (u === ZONE_READINGS_READ) return new Response(JSON.stringify({ data: [zoneRow()], total: 1 }), { status: 200 });
      if (u === ZONE_OPEN_READ) return new Response('', { status: 503 });
      return new Response(JSON.stringify({ data: [], total: 0 }), { status: 200 });
    }) as unknown as typeof fetch;
    const s = await fetchEstate();
    expect(asked.filter((u) => u.includes(`kind=${ZONE_KIND}`)).sort()).toEqual([ZONE_READINGS_READ, ZONE_OPEN_READ].sort());
    expect(s.zones.latest.kind).toBe('ready');
    if (s.zones.latest.kind === 'ready') expect(s.zones.latest.data.map((z) => z.zone)).toEqual(['example.org']);
    expect(s.zones.open).toEqual({ kind: 'failed', error: `${ZONE_OPEN_READ}: HTTP 503` });
  });
});

describe('EstatePage renders the door from the module', () => {
  // Source-level pin, the TriageBoard posture: bun test has no Svelte
  // pass, and the coupling this guards against — an address or a
  // command typed into the page — would appear in the source, not in a
  // render.
  const source = readFileSync(new URL('./EstatePage.svelte', import.meta.url), 'utf8');
  const code = source
    .replace(/<!--[\s\S]*?-->/g, '')
    .replace(/\/\*[\s\S]*?\*\//g, '')
    .replace(/(^|[^:])\/\/.*$/gm, '$1');

  test('carries no address of its own — every IP came from the registry node, and none is left', () => {
    expect(code).not.toMatch(/\b\d{1,3}(?:\.\d{1,3}){3}\b/);
  });

  test('the setup block is rendered from devDoorSteps, not retyped', () => {
    expect(code).toMatch(/devDoorSteps\(\)/);
    expect(code).toMatch(/\{#each\s+doorSteps\b/);
    expect(code).toMatch(/\{step\.command\}/);
    expect(code).not.toMatch(/cloudflared access ssh-config/);
  });

  test('the bastion route is gone with the door it served', () => {
    expect(code).not.toMatch(/bastion/i);
    expect(code).not.toMatch(/ProxyJump/);
  });
});

// A REFUSAL IS NOT AN OUTAGE (backlog e5f7b51e): the estate reads ask
// policy, so a 401/403 names the session, never a registry that did not answer.
describe('registryFailure', () => {
  test('a 403 or 401 says the registry refused this session', () => {
    for (const code of [401, 403]) {
      const said = registryFailure(`/api/estate/nodes: HTTP ${code}`);
      expect(said).toStartWith('The registry refused this session: /api/estate/nodes: HTTP ');
      expect(said).not.toContain('did not answer');
    }
  });
  test('anything else is the unreachable registry it always was', () => {
    expect(registryFailure('/api/estate/nodes: HTTP 503')).toBe(
      'The registry did not answer: /api/estate/nodes: HTTP 503. This page refuses to guess — an unreachable registry is not an empty estate.',
    );
  });
});

// THE INSTANCE VOLUMES (backlog 21ee3b4e, incident d3c0a67c): every
// claim the forge read, judged by estate.compare, beside the machines.
describe('the instance volumes', () => {
  const GIB = 1024 ** 3;
  const vol = (over: Record<string, unknown>): Record<string, unknown> => ({
    id: 'boss/pgdata-postgres-0', namespace: 'boss', claim: 'pgdata-postgres-0',
    volume: 'pvc-93e11a6e-6999-41a8-9df3-622f36b7ff56',
    capacity_bytes: 30 * GIB, used_bytes: 25 * GIB, free_bytes: 5 * GIB, floor_bytes: 6 * GIB, tight: true,
    ...over,
  });
  const cmpEvent = (observed_at: string, volumes: unknown[], scope = VOLUMES_SCOPE): unknown => ({
    payload: { scope, observed_at, observer: 'boss-estate-observe-volumes', counts: {}, findings: {}, volumes },
  });

  test('the volumes are read as their own judged series, scoped', () => {
    expect(VOLUMES_SCOPE).toBe('instance-volumes');
    expect(VOLUMES_READ).toBe('/api/estate/comparisons?scope=instance-volumes&limit=10');
  });

  test('a volume under its floor reads tight, with its free, capacity and floor', () => {
    const [r] = parseVolumeReadings({ data: [cmpEvent('2026-10-01T16:00:00Z', [vol({})])], total: 1 });
    expect(r?.volumes[0]?.claim).toBe('pgdata-postgres-0');
    expect(volumeLine(r!.volumes[0]!)).toEqual({ state: 'tight', text: '5.0G free of 30.0G — under its 6.0G floor' });
  });

  test('a volume above its floor reads its headroom and the floor it is judged by', () => {
    const [r] = parseVolumeReadings([cmpEvent('2026-10-01T16:00:00Z', [vol({ free_bytes: 20 * GIB, tight: false })])]);
    expect(volumeLine(r!.volumes[0]!)).toEqual({ state: 'ok', text: '20.0G free of 30.0G · floor 6.0G' });
  });

  test('a volume that could not be read says unread and why, never fine', () => {
    const unread = vol({
      id: 'boss/boss-files', claim: 'boss-files', capacity_bytes: null, used_bytes: null, free_bytes: null,
      floor_bytes: null, tight: null, unread: 'the kubelet stats of w-2 could not be read',
    });
    const [r] = parseVolumeReadings([cmpEvent('2026-10-01T16:00:00Z', [unread])]);
    expect(volumeLine(r!.volumes[0]!)).toEqual({ state: 'unread', text: 'unread: the kubelet stats of w-2 could not be read' });
    // A row that carries a verdict but no figures is not taken at its word.
    const hollow = parseVolumeReadings([cmpEvent('2026-10-01T16:00:00Z', [vol({ free_bytes: null, tight: false })])]);
    expect(volumeLine(hollow[0]!.volumes[0]!).state).toBe('unread');
  });

  test('only this scope\'s rows are kept, newest first as read', () => {
    const rows = parseVolumeReadings([
      cmpEvent('2026-10-01T16:00:00Z', [vol({})]),
      cmpEvent('2026-10-01T16:00:00Z', [vol({})], 'host'),
      cmpEvent('2026-10-01T15:45:00Z', [vol({})]),
    ]);
    expect(rows.map((r) => r.observed_at)).toEqual(['2026-10-01T16:00:00Z', '2026-10-01T15:45:00Z']);
  });

  test('a volume\'s alarm is the raiser\'s key for its claim — disk_tight or blind to it', () => {
    const page = parseAlarms({
      data: [
        { id: 'a1', title: 't', metadata: { estate_finding: 'disk_tight:boss/pgdata-postgres-0', scope: VOLUMES_SCOPE } },
        { id: 'a2', title: 't', metadata: { estate_finding: 'blind:disk_tight/boss/boss-files', scope: VOLUMES_SCOPE } },
        { id: 'a3', title: 't', metadata: { estate_finding: 'disk_tight:boss-gcp', scope: 'host' } },
      ],
      total: 3,
    });
    const ready = { kind: 'ready', data: page } as const;
    expect(volumeAlarms(ready, 'boss/pgdata-postgres-0').map((a) => a.id)).toEqual(['a1']);
    expect(volumeAlarms(ready, 'boss/boss-files').map((a) => a.id)).toEqual(['a2']);
  });

  test('fetchEstate reads the volumes, and a failed read lands failed, never as no volumes', async () => {
    const asked: string[] = [];
    globalThis.fetch = (async (url: RequestInfo | URL) => {
      const u = String(url);
      asked.push(u);
      if (u === VOLUMES_READ) return new Response('{}', { status: 503 });
      return new Response(JSON.stringify(u.includes('/nodes') ? [node()] : []), { status: 200 });
    }) as unknown as typeof fetch;
    const s = await fetchEstate();
    expect(asked).toContain(VOLUMES_READ);
    expect(s.volumes.kind).toBe('failed');
  });

  test('the floor is never computed on the page: the comparator\'s verdict is what it draws', () => {
    const page = readFileSync(new URL('./EstatePage.svelte', import.meta.url), 'utf8');
    expect(page).toMatch(/volumeLine\(/);
    expect(page).not.toMatch(/\b0\.2\b|\b20 ?%|4 ?GiB/);
  });
});
