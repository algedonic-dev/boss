// The one answer to "may this value be a link?" (backlog 4f1f7698,
// 2026-09-27).
//
// WHY. Svelte does not sanitise `href` or `src`, and CodeQL does not see
// through a Svelte template, so a URL read off a record and bound
// straight into `<a href={doc.url}>` runs as script when the record
// holds `javascript:alert(1)` and a user clicks it — the same bug the
// diagnostic-call plugin's Join button had (4a359b51). Six pages bound
// stored URLs that way (device and part documents, KB documents,
// marketing asset files, the yard's PR links).
//
// So every dynamic `href=` and `src=` under apps/web, libs/web-kit and
// apps/simulator goes through this function, and
// apps/web/src/linkSinks.test.ts refuses one that does not. A value is
// a link only when it is:
//   - an absolute http(s) URL, or
//   - a site-relative path: one `/` followed by anything but a second
//     `/` or a `\` — `//host` and `/\host` are other sites to a browser
//     (protocol-relative; WHATWG parsing treats `\` as `/` for http).
// Anything else — another scheme, a relative path, a value with a
// control character anywhere (a browser strips tab and newline out of
// a URL, so `/\t/host` IS `//host`) or leading whitespace — answers
// null, and the page draws the value as text instead of a link.

// C0 controls and DEL. A browser removes tab/CR/LF from anywhere in a
// URL and trims the rest from its ends, so any of them can turn a
// value that passed the checks below into one that would not have.
const CONTROL = /[\u0000-\u001f\u007f]/;

const ABSOLUTE_HTTP = /^https?:\/\//i;

/// The value as a link target, or null when it must be drawn as text.
export function safeLinkHref(value: unknown): string | null {
  if (typeof value !== 'string' || value === '') return null;
  if (CONTROL.test(value) || /^\s/.test(value)) return null;
  if (ABSOLUTE_HTTP.test(value)) return value;
  if (value.startsWith('/')) {
    const second = value.charAt(1);
    return second === '/' || second === '\\' ? null : value;
  }
  return null;
}

/// The value as a path on THIS site, or null. For a redirect target a
/// visitor can write (`/login?next=`, a step page's `?from=`), where an
/// absolute http(s) URL is an open redirect even though it is not script.
export function safeSitePath(value: unknown): string | null {
  const link = safeLinkHref(value);
  return link !== null && link.startsWith('/') ? link : null;
}
