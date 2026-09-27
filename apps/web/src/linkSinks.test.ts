// No Svelte template binds a value into href= or src= unless the value
// is safeLinkHref's answer or its scheme is written in the source
// (backlog 4f1f7698, 2026-09-27).
//
// WHY. Svelte does not sanitise href or src, and CodeQL does not see
// through a Svelte template, so `<a href={doc.url}>` ran a stored
// `javascript:` URL on a click — on the device, part, KB and marketing
// asset pages and the yard's PR links, the same bug the diagnostic-call
// Join button had (4a359b51). The helper (@boss/web-kit/links) answers
// http(s) or a site-relative path and null otherwise; a null href drops
// the attribute, so the element is no longer a link.
//
// A dynamic value passes here in exactly two shapes:
//   - `safeLinkHref(...)`, the whole expression; or
//   - a literal whose origin is fixed in the source: a template or quoted
//     value (optionally the whole argument of the router's `href(...)`,
//     which only prepends a mount prefix) that begins `/` plus a
//     character other than `/`, `\` or an interpolation, or `mailto:`.
//     Such a value is site-relative (or a mail link) whatever it
//     interpolates, so wrapping it would test nothing.
// Everything else — a variable, a field, a helper's return, a ternary —
// goes through safeLinkHref, and a new one that does not is named below.

import { describe, expect, test } from 'bun:test';
import { readdirSync, readFileSync, statSync } from 'node:fs';
import { join, relative } from 'node:path';

const REPO = new URL('../../../', import.meta.url).pathname;
const ROOTS = ['apps/web/src', 'libs/web-kit/src', 'apps/simulator/src'];

function svelteFiles(dir: string): string[] {
  return readdirSync(dir).flatMap((name) => {
    const p = join(dir, name);
    if (statSync(p).isDirectory()) return svelteFiles(p);
    return p.endsWith('.svelte') ? [p] : [];
  });
}

/// Blank out script, style and comment blocks, keeping their newlines so
/// line numbers still point at the file.
function templateOnly(source: string): string {
  const blank = (m: string) => m.replace(/[^\n]/g, ' ');
  return source
    .replace(/<script\b[\s\S]*?<\/script>/g, blank)
    .replace(/<style\b[\s\S]*?<\/style>/g, blank)
    .replace(/<!--[\s\S]*?-->/g, blank);
}

/// Index just past the brace that closes the one opening at `open`.
function closeBrace(s: string, open: number): number {
  let depth = 0;
  for (let i = open; i < s.length; i++) {
    const c = s[i];
    if (c === '{') depth++;
    else if (c === '}' && --depth === 0) return i + 1;
  }
  return s.length;
}

/// Index just past the literal starting at `i` (a quote or a backtick),
/// or -1 when `i` does not start one. A template's `${...}` is skipped.
function literalEnd(s: string, i: number): number {
  const q = s[i];
  if (q !== '`' && q !== "'" && q !== '"') return -1;
  for (let j = i + 1; j < s.length; j++) {
    if (s[j] === '\\') {
      j++;
    } else if (q === '`' && s[j] === '$' && s[j + 1] === '{') {
      j = closeBrace(s, j + 1) - 1;
    } else if (s[j] === q) {
      return j + 1;
    }
  }
  return -1;
}

/// The literal text begins with a fixed origin: one `/` then something
/// that is not `/`, `\` or an interpolation — or a mailto: scheme.
function fixedOrigin(text: string): boolean {
  return /^\/(?![/\\$]|\{)/.test(text) || text.startsWith('mailto:') || text === '/';
}

/// Does `expr` (the inside of `href={...}`) pass?
function expressionPasses(raw: string): boolean {
  const expr = raw.trim();
  if (expr.startsWith('safeLinkHref(')) {
    // The call must be the whole expression, not the left arm of `??`.
    let depth = 0;
    for (let i = 'safeLinkHref'.length; i < expr.length; i++) {
      if (expr[i] === '(') depth++;
      else if (expr[i] === ')' && --depth === 0) return i === expr.length - 1;
    }
    return false;
  }
  // A multi-line call carries a trailing comma after its one argument.
  const inner = /^href\(([\s\S]*)\)$/.exec(expr)?.[1]?.trim().replace(/,$/, '') ?? expr;
  const end = literalEnd(inner, 0);
  return end === inner.length && fixedOrigin(inner.slice(1));
}

type Sink = Readonly<{ file: string; line: number; text: string; passes: boolean }>;

/// Every href=/src= in one template whose value is not a plain literal.
function sinks(file: string, source: string): Sink[] {
  const t = templateOnly(source);
  const lineOf = (i: number) => t.slice(0, i).split('\n').length;
  const out: Sink[] = [];
  const attr = /(?<![\w-])(href|src)=/g;
  for (let m = attr.exec(t); m; m = attr.exec(t)) {
    const at = m.index + m[0].length;
    const c = t[at];
    if (c === '{') {
      const end = closeBrace(t, at);
      const expr = t.slice(at + 1, end - 1);
      out.push({ file, line: lineOf(m.index), text: `${m[0]}{${expr.trim()}}`, passes: expressionPasses(expr) });
    } else if (c === '"' || c === "'") {
      const end = t.indexOf(c, at + 1);
      const value = t.slice(at + 1, end);
      if (value.includes('{')) {
        out.push({ file, line: lineOf(m.index), text: `${m[0]}${c}${value}${c}`, passes: fixedOrigin(value) });
      }
    }
  }
  // The shorthand attribute `{href}` / `{src}`: a bare variable.
  const shorthand = /(?<=\s)\{(href|src)\}(?=[\s/>])/g;
  for (let m = shorthand.exec(t); m; m = shorthand.exec(t)) {
    out.push({ file, line: lineOf(m.index), text: m[0], passes: false });
  }
  return out;
}

const FILES = ROOTS.flatMap((r) => svelteFiles(join(REPO, r))).sort();
const ALL = FILES.flatMap((p) => sinks(relative(REPO, p), readFileSync(p, 'utf8')));

describe('every dynamic href and src goes through safeLinkHref', () => {
  test('the templates are there to read', () => {
    // A wrong root reads zero files and passes everything below.
    expect(FILES.length).toBeGreaterThan(100);
    expect(ALL.some((s) => s.file === 'apps/web/src/catalog/DevicePage.svelte')).toBe(true);
  });

  test('no template binds a value the helper has not judged', () => {
    const refused = ALL.filter((s) => !s.passes).map((s) => `${s.file}:${s.line}: ${s.text}`);
    expect(refused).toEqual([]);
  });

  test('the check refuses the shapes that carry a scheme from data', () => {
    for (const bad of [
      '<a href={doc.url}>x</a>',
      '<a href={t.pr_url} target="_blank">PR</a>',
      '<img src={src} alt="" />',
      '<a {href}>x</a>',
      '<a href="{url}">x</a>',
      '<a href="//{host}/x">x</a>',
      '<a href={href(path)}>x</a>',
      "<a href={cond ? '/ux/a' : doc.url}>x</a>",
      '<a href={safeLinkHref(doc.url) ?? doc.url}>x</a>',
      '<a href={`${base}/x`}>x</a>',
      '<a href={`//${host}`}>x</a>',
      '<a\n  href={\n    item.path\n  }>x</a>',
    ]) {
      expect(sinks('fixture.svelte', bad).map((s) => s.passes)).toEqual([false]);
    }
  });

  test('the check passes the helper and fixed-origin literals', () => {
    for (const good of [
      '<a href={safeLinkHref(doc.url)}>x</a>',
      '<a href={safeLinkHref(href(`/jobs/${id}`))}>x</a>',
      '<a href={`/ux/jobs/${id}`}>x</a>',
      '<a href={href(`/jobs/${j.id}`)}>x</a>',
      "<a href={href('/it/registry')}>x</a>",
      '<a href={`mailto:${email}`}>x</a>',
      '<a href="/jobs/{id}">x</a>',
    ]) {
      expect(sinks('fixture.svelte', good).map((s) => s.passes)).toEqual([true]);
    }
    // Script and comment text is not template.
    expect(sinks('f.svelte', '<script>const a = `<a href={x}>`;</script><!-- href={y} -->')).toEqual([]);
  });
});
