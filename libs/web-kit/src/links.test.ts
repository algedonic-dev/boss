// safeLinkHref is the one gate between a stored value and a link
// (backlog 4f1f7698). Run via `bun test` from libs/web-kit — plain
// TypeScript, no runes, so no preload is needed.

import { describe, expect, test } from 'bun:test';
import { safeLinkHref, safeSitePath } from './links';

describe('safeLinkHref — http(s) and site-relative only', () => {
  test('absolute http(s) URLs pass unchanged, any case', () => {
    for (const v of [
      'https://example.com/doc.pdf',
      'http://example.com',
      'HTTPS://Example.com/a?b=c#d',
      'https://forge.example/owner/repo/pulls/12',
    ]) {
      expect(safeLinkHref(v)).toBe(v);
    }
  });

  test('site-relative paths pass unchanged', () => {
    for (const v of ['/', '/ux/jobs/abc', '/jobs?kind=x&status=closed', '/dashboard/ux/me', '/a//b']) {
      expect(safeLinkHref(v)).toBe(v);
    }
  });

  test('script, data and every other scheme is refused', () => {
    for (const v of [
      'javascript:alert(1)',
      'JavaScript:alert(1)',
      'data:text/html,<script>alert(1)</script>',
      'vbscript:msgbox(1)',
      'file:///etc/passwd',
      'mailto:a@example.com',
      'ftp://example.com',
      'blob:https://example.com/x',
    ]) {
      expect(safeLinkHref(v)).toBeNull();
    }
  });

  test('protocol-relative and backslash forms are another site, refused', () => {
    for (const v of ['//evil.example', '//evil.example/path', '/\\evil.example', '/\\/evil.example']) {
      expect(safeLinkHref(v)).toBeNull();
    }
  });

  test('a control character anywhere, or leading whitespace, is refused', () => {
    // A browser strips tab/newline from inside a URL: `/\t/evil` is `//evil`,
    // and `java\tscript:` is `javascript:`.
    for (const v of [
      '/\t/evil.example',
      '/\n/evil.example',
      'java\tscript:alert(1)',
      ' javascript:alert(1)',
      ' /ux/jobs',
      '\u0000javascript:alert(1)',
      'https://example.com/\u0001',
      ' https://example.com',
    ]) {
      expect(safeLinkHref(v)).toBeNull();
    }
  });

  test('relative paths, bare words and non-strings are refused', () => {
    for (const v of ['', 'ux/jobs', './a', '../a', 'example.com', '#top', '?q=1', null, undefined, 42, {}]) {
      expect(safeLinkHref(v)).toBeNull();
    }
  });
});

describe('safeSitePath — a redirect target stays on this site', () => {
  test('a site path passes; every other site, and every scheme, is refused', () => {
    expect(safeSitePath('/ux/jobs?x=1')).toBe('/ux/jobs?x=1');
    for (const v of [
      'https://evil.example',
      '//evil.example',
      '/\\evil.example',
      '/\t/evil.example',
      'javascript:alert(1)',
      'evil',
      null,
    ]) {
      expect(safeSitePath(v)).toBeNull();
    }
  });
});
