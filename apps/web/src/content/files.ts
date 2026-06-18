// File-references — types + fetch helpers for /api/files. Mirrors
// boss-content::files types so the SPA can render attachments without
// guessing field shapes.
//
// Files are not a new domain, they are a new column on every
// existing primitive (Subject, Job, Step, Event) — see
// docs/architecture-decisions.md §Content, files, knowledge. One
// <FileAttachments /> Svelte component handles every target via
// these helpers.

export type ResourceKind = 'subject' | 'job' | 'step' | 'event';

export type FileRef = {
  id: string;
  target: { kind: ResourceKind; id: string };
  bucket: string;
  object_key: string;
  sha256: string;
  size_bytes: number;
  mime: string;
  filename: string;
  uploaded_by: string;
  uploaded_at: string;
  deleted_at: string | null;
};

/** Result of listing files. The `unconfigured` variant fires when
 *  boss-content-api signals its `[files]` config block is missing
 *  (since 2026-05-22 the server returns 200 with
 *  `{kind: "unconfigured"}` instead of 503, so auditor-role browsing
 *  sessions don't surface a network-tab error for a designed-off
 *  surface). Older deployments still 503; we handle both. */
export type ListFilesResult =
  | { kind: 'ok'; files: ReadonlyArray<FileRef> }
  | { kind: 'unconfigured' };

export async function listFilesFor(
  kind: ResourceKind,
  id: string,
): Promise<ListFilesResult> {
  const url = `/api/files?target_kind=${encodeURIComponent(kind)}&target_id=${encodeURIComponent(id)}`;
  const r = await fetch(url, { credentials: 'same-origin' });
  if (r.status === 503) {
    return { kind: 'unconfigured' };
  }
  if (!r.ok) {
    throw new Error(`list files: HTTP ${r.status}`);
  }
  const body = (await r.json()) as
    | ReadonlyArray<FileRef>
    | { kind: 'unconfigured'; reason?: string };
  if (Array.isArray(body)) {
    return { kind: 'ok', files: body };
  }
  return { kind: 'unconfigured' };
}

/// All uploads go through the multipart `POST /api/files` handler.
/// The local-disk storage backend streams bytes through the
/// content-api (there's no presigned direct-to-bucket path), so a
/// single code path covers every file size.
export async function uploadFile(
  kind: ResourceKind,
  id: string,
  file: File,
): Promise<FileRef> {
  const form = new FormData();
  form.append('target_kind', kind);
  form.append('target_id', id);
  form.append('file', file, file.name);
  const r = await fetch('/api/files', {
    method: 'POST',
    body: form,
    credentials: 'same-origin',
  });
  if (!r.ok) {
    const body = await r.text().catch(() => '');
    throw new Error(`upload: HTTP ${r.status}${body ? ` — ${body}` : ''}`);
  }
  return (await r.json()) as FileRef;
}

export async function deleteFile(id: string): Promise<void> {
  const r = await fetch(`/api/files/${encodeURIComponent(id)}`, {
    method: 'DELETE',
    credentials: 'same-origin',
  });
  if (!r.ok && r.status !== 404) {
    throw new Error(`delete: HTTP ${r.status}`);
  }
}

export function downloadHref(id: string): string {
  return `/api/files/${encodeURIComponent(id)}`;
}

/// Format byte counts the way file managers do — "4.2 KB", "1.3 MB".
/// Pure UI helper; mirrors the convention used in the inventory pages
/// so attachment sizes look familiar across surfaces.
export function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  const units = ['KB', 'MB', 'GB', 'TB'] as const;
  let i = -1;
  let v = n;
  do {
    v /= 1024;
    i += 1;
  } while (v >= 1024 && i < units.length - 1);
  return `${v.toFixed(v >= 10 ? 0 : 1)} ${units[i]}`;
}

/// Image MIMEs render as inline thumbnails; everything else gets a
/// generic icon. Match the design doc's UI shape note (line 149).
export function isImage(mime: string): boolean {
  return mime.startsWith('image/');
}
