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

/// Files > 50 MiB use the two-phase presigned-URL upload path
/// (`/api/files/_upload-url` → bucket PUT → `/api/files/_finalize`)
/// so the bytes don't stream through the gateway. Smaller files go
/// through the multipart `POST /api/files` handler.
const LARGE_UPLOAD_THRESHOLD_BYTES = 50 * 1024 * 1024;

export async function uploadFile(
  kind: ResourceKind,
  id: string,
  file: File,
): Promise<FileRef> {
  if (file.size > LARGE_UPLOAD_THRESHOLD_BYTES) {
    return uploadFileLarge(kind, id, file);
  }
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

/// Two-phase large-file upload. Hashes the file in the browser via
/// `crypto.subtle.digest('SHA-256', ...)`, requests a presigned PUT
/// URL keyed by that hash, PUTs the bytes directly to the bucket,
/// then asks the service to finalize (insert row + emit event).
async function uploadFileLarge(
  kind: ResourceKind,
  id: string,
  file: File,
): Promise<FileRef> {
  const sha256 = await sha256OfFile(file);
  const mime = file.type || 'application/octet-stream';
  const meta = {
    target_kind: kind,
    target_id: id,
    sha256,
    size_bytes: file.size,
    mime,
    filename: file.name,
  };
  const urlResp = await fetch('/api/files/_upload-url', {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(meta),
    credentials: 'same-origin',
  });
  if (!urlResp.ok) {
    const body = await urlResp.text().catch(() => '');
    throw new Error(`upload-url: HTTP ${urlResp.status}${body ? ` — ${body}` : ''}`);
  }
  const presigned = (await urlResp.json()) as {
    file_id: string;
    upload_url: string;
    object_key: string;
    expires_in_secs: number;
  };

  // Direct browser → bucket PUT. The presigned URL bound the
  // Content-Type into its signature, so we MUST send the same mime
  // back or the bucket rejects with SignatureDoesNotMatch.
  const putResp = await fetch(presigned.upload_url, {
    method: 'PUT',
    headers: { 'content-type': mime },
    body: file,
  });
  if (!putResp.ok) {
    throw new Error(`bucket PUT: HTTP ${putResp.status}`);
  }

  const finalizeResp = await fetch('/api/files/_finalize', {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ file_id: presigned.file_id, ...meta }),
    credentials: 'same-origin',
  });
  if (!finalizeResp.ok) {
    const body = await finalizeResp.text().catch(() => '');
    throw new Error(`finalize: HTTP ${finalizeResp.status}${body ? ` — ${body}` : ''}`);
  }
  return (await finalizeResp.json()) as FileRef;
}

async function sha256OfFile(file: File): Promise<string> {
  // crypto.subtle.digest streams the whole buffer through the
  // browser's WebCrypto implementation. For files in the 50 MiB –
  // few-GiB range this is fine; it's the same shape as the
  // server-side sha2 hash so the keys agree end-to-end.
  const buf = await file.arrayBuffer();
  const hashBuf = await crypto.subtle.digest('SHA-256', buf);
  return Array.from(new Uint8Array(hashBuf))
    .map((b) => b.toString(16).padStart(2, '0'))
    .join('');
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
