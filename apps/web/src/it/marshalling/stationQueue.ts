import { queueViewFromBody, type StationQueueView } from '../monitoring/stationMap';

const record = (value: unknown): value is Record<string, unknown> =>
  value !== null && typeof value === 'object' && !Array.isArray(value);

/** A queue is evidence about the selected station, never a fallback
 *  empty list or a client-sorted reconstruction (fdc0ea0b). */
export function parseStationQueue(body: unknown, station: string): StationQueueView {
  if (!record(body) || body.station !== station || !Array.isArray(body.data)
    || !Number.isSafeInteger(body.total) || (body.total as number) < body.data.length
    || !Array.isArray(body.discipline) || body.discipline.length === 0
    || !body.discipline.every((key) => typeof key === 'string' && key.trim().length > 0)
    || !body.data.every((job) => record(job)
      && typeof job.id === 'string' && job.id.length > 0
      && typeof job.kind === 'string' && job.kind.length > 0
      && typeof job.title === 'string' && typeof job.opened_on === 'string'
      && (job.tags == null || (Array.isArray(job.tags) && job.tags.every((tag) => typeof tag === 'string')))
      && (job.metadata == null || record(job.metadata)))
    || new Set(body.data.map((job) => job.id)).size !== body.data.length) {
    throw new Error(`The ${station} queue did not supply matching station, discipline, total and packet evidence`);
  }
  const queue = queueViewFromBody(body);
  if (queue === null) throw new Error(`The ${station} queue was not an envelope`);
  return queue;
}
