// THE MAP'S POLL — one read in flight per endpoint (backlog 452e507c,
// 909adf3b). MapPage used to start all four reads on every 10 s
// setInterval tick whether or not the last ones had landed, and drew
// nothing until the slowest answered (Promise.all). Measured 2026-10-21
// (run d740cf7e): regions at 26 s and borders at 23 s on a quiet API
// stacked to 127-187 s, every read past 100 s was a Cloudflare 524, and
// each cancelled request kept computing on the server. Now a tick skips
// an endpoint whose read is still out, each endpoint publishes the moment
// it answers with its OWN completion time (a slow borders read never
// holds the stations, nor renews the regions' "read Ns ago"), and a
// hidden tab adds no background reads — it reads again on its return.
// The mount read runs even in a hidden tab: a page opened in the
// background still needs one map to draw (the it-map-live hidden-tab
// spec counts moves on it; gate 2b1594ef went red without it).

type Failed = { kind: 'failed'; error: string };
type Publication = (at: number) => void;
type Read = () => Promise<Publication>;

/** Keep each endpoint's value and timestamp together, including a failed read. */
export function mapRead<T>(read: () => Promise<T>, publish: (value: T | Failed, at: number) => void): Read {
  return async () => {
    let value: T | Failed;
    try { value = await read(); }
    catch (error) { value = { kind: 'failed', error: error instanceof Error ? error.message : String(error) }; }
    return (at) => publish(value, at);
  };
}

type PollHost = Readonly<{
  hidden: () => boolean;
  every: (tick: () => void, ms: number) => () => void;
  onVisibility: (tick: () => void) => () => void;
}>;

export function startMapPolling(reads: ReadonlyArray<Read>, host: PollHost, now = Date.now): () => void {
  let disposed = false;
  const pending = new Set<Read>();
  const start = () => {
    for (const read of reads) {
      if (pending.has(read)) continue;
      pending.add(read);
      void read().then((publish) => {
        if (!disposed) publish(now());
      }).finally(() => pending.delete(read));
    }
  };
  const tick = () => {
    if (disposed || host.hidden()) return;
    start();
  };
  start();
  const stop = host.every(tick, 10_000);
  const removeVisibility = host.onVisibility(tick);
  return () => { disposed = true; stop(); removeVisibility(); };
}
