// No step plugin hands page text to a sink that parses HTML (backlog
// 4a359b51, 2026-09-27).
//
// WHY. GitHub CodeQL on publish PR #245 (check-run 108548538737) failed
// the publish on two high-severity alerts: sign-off.js:84, "DOM text is
// reinterpreted as HTML" (two flows) plus "exception text reinterpreted
// as HTML" at the same line, and diagnostic-call.js:137, "DOM text is
// reinterpreted as HTML". Line 84 was the h() helper's
// `el.appendChild(child instanceof Node ? child : createTextNode(...))`:
// the ternary hands appendChild the RAW child on one arm, and the
// analyser reads appendChild's argument as HTML without believing the
// instanceof. Line 137 set the Join button's href from the URL box
// verbatim, so a `javascript:` join_url in step metadata ran in the
// operator's session on a click. These plugins render the approval and
// sign-off steps a passkey signs.
//
// So every plugin under infra/step-plugins/ is read here and held to
// two rules. Its h() helper appends through `append`, whose string
// argument the DOM makes a Text node by definition — never a ternary
// into appendChild. And nothing parses a string as HTML, except the
// sites named below, each with the reason it is safe: a renderer whose
// contract is escape-first, or a sandboxed frame. The behaviour — a
// title and a metadata value carrying an <img onerror> drawn as their
// characters — is driven in a real browser by
// tests/mocked/step-plugins-draw-page-text-as-text.mocked.spec.ts.

//
// WIDENED (backlog 4f1f7698, the security review of that car): the HTML
// scan is a bare-word match, and the URL sinks are held to the same
// shape — a link or a source is a site path literal or a named site,
// because a `javascript:` href runs on a click. The pages' own links are
// held by src/linkSinks.test.ts and @boss/web-kit/links.

import { describe, expect, test } from 'bun:test';
import { readdirSync, readFileSync } from 'node:fs';

const DIR = new URL('../../../../infra/step-plugins/', import.meta.url);

const PLUGINS = readdirSync(DIR)
  .filter((f) => f.endsWith('.js'))
  .sort()
  .map((file) => ({ file, source: readFileSync(new URL(file, DIR), 'utf8') }));

// A property, method or attribute that turns a string into markup — as a
// bare word, so `el['innerHTML'] =` and a DOMParser are caught as well
// as the plain assignment (backlog 4f1f7698).
const HTML_SINK =
  /\b(innerHTML|outerHTML|insertAdjacentHTML|setHTMLUnsafe|parseFromString|srcdoc|createContextualFragment)\b|document\.write/;

// An attribute or property that navigates or loads: setAttribute('href'
// | 'src'), a `.href =` / `.src =` / `['href'] =` write, or an `href:` /
// `src:` prop handed to h(), whose loop sets it with setAttribute.
const URL_SINK =
  /setAttribute\(\s*['"](href|src)['"]|(\.(href|src)|\[\s*['"](href|src)['"]\s*\])\s*=(?!=)|\b(href|src)\s*:/;

// The value is a literal that starts with one `/`: a path on this site,
// whatever it interpolates, so the scheme is not the data's to choose.
const SITE_PATH_VALUE =
  /(setAttribute\(\s*['"](href|src)['"]\s*,\s*|\b(href|src)\s*[:=]\s*)[`'"]\/(?![/\\]|\$\{)/;

// Each allowed site, as the exact line it is, and why it stays.
const ALLOWED: ReadonlyArray<Readonly<{ file: string; line: string; why: string }>> = [
  {
    file: 'answer-question.js',
    line: 'inner.innerHTML = renderMarkdown(ctx);',
    why: 'the bundle-local renderMarkdown escapes the whole text first and emits only its own tags, links http(s) only',
  },
  {
    file: 'sign-off.js',
    line: 'body.innerHTML = render(text);',
    why: 'window.__boss_markdown, the host escape-first renderer (2244db9e); absent, the case is textContent',
  },
  {
    file: 'review-design.js',
    line: 'prose.innerHTML = render(doc.markdown);',
    why: 'window.__boss_markdown, the host escape-first renderer (2244db9e); absent, the prose is textContent',
  },
  {
    file: 'review-design.js',
    line: "frame.setAttribute('srcdoc', exhibitSrcdoc(ex.html));",
    why: 'an exhibit renders only in a frame sandboxed to allow-scripts, opaque origin, CSP default-src none (26a89f11)',
  },
  {
    file: 'diagnostic-call.js',
    line: "joinBtn.setAttribute('href', url);",
    why: 'url is meetingHref(), which parses the box and writes the https:// or http:// scheme as a literal, else null (4a359b51)',
  },
];

// A comment line names a sink to explain it; it is not one.
const COMMENT = /^(\/\/|\/\*|\*)/;

function sinkLines(file: string, source: string): string[] {
  return source
    .split('\n')
    .map((l) => l.trim())
    .filter((l) => !COMMENT.test(l) && HTML_SINK.test(l))
    .filter((l) => !ALLOWED.some((a) => a.file === file && a.line === l));
}

function urlSinkLines(file: string, source: string): string[] {
  return source
    .split('\n')
    .map((l) => l.trim())
    .filter((l) => !COMMENT.test(l) && URL_SINK.test(l) && !SITE_PATH_VALUE.test(l))
    .filter((l) => !ALLOWED.some((a) => a.file === file && a.line === l));
}

describe('step plugins draw page text as text', () => {
  test('the plugins are there to read', () => {
    // A wrong directory reads zero files and passes everything below.
    expect(PLUGINS.map((p) => p.file)).toContain('sign-off.js');
    expect(PLUGINS.map((p) => p.file)).toContain('diagnostic-call.js');
  });

  test('no plugin parses a string as HTML outside the named sites', () => {
    const found = PLUGINS.flatMap(({ file, source }) =>
      sinkLines(file, source).map((l) => `${file}: ${l}`),
    );
    expect(found).toEqual([]);
  });

  test('the HTML scan catches every spelling, not only the assignment', () => {
    // A bare-word match (backlog 4f1f7698): `el['innerHTML'] =`, a
    // DOMParser, setHTMLUnsafe and a srcdoc property all parse markup,
    // and the assignment-only pattern saw none of the first three.
    for (const planted of [
      "el['innerHTML'] = text;",
      "const doc = new DOMParser().parseFromString(text, 'text/html');",
      'el.setHTMLUnsafe(text);',
      'frame.srcdoc = text;',
      "x.insertAdjacentHTML('beforeend', text);",
      'el.outerHTML = text;',
    ]) {
      expect(sinkLines('fixture.js', planted)).toEqual([planted]);
    }
    expect(sinkLines('fixture.js', '// innerHTML is named here to explain it')).toEqual([]);
  });

  test('no plugin sets a link or a source from a value outside the named sites', () => {
    // A `javascript:` href runs on a click as surely as markup runs on
    // parse: the Join button's href (diagnostic-call.js, 4a359b51) was
    // this shape, so the URL sinks are scanned too — setAttribute('href'
    // | 'src'), a `.href =` / `.src =` write, and an `href:` / `src:`
    // prop handed to a plugin's h() helper, which sets it as an
    // attribute. A literal that starts with one `/` is a path on this
    // site whatever it interpolates, so it passes.
    const found = PLUGINS.flatMap(({ file, source }) =>
      urlSinkLines(file, source).map((l) => `${file}: ${l}`),
    );
    expect(found).toEqual([]);
  });

  test('the URL scan refuses a value and passes a site path', () => {
    for (const planted of [
      "a.setAttribute('href', meta.url);",
      'img.setAttribute("src", meta.image);',
      'a.href = meta.url;',
      'img.src = meta.image;',
      "a['href'] = meta.url;",
      "h('a', { href: meta.url }, 'open')",
      "h('a', { href: `//${host}/x` }, 'open')",
    ]) {
      expect(urlSinkLines('fixture.js', planted)).toEqual([planted]);
    }
    for (const safe of [
      "h('a', { href: `/ux/jobs/${id}` }, id)",
      "a.setAttribute('href', '/ux/jobs');",
      'if (a.href === b.href) return;',
      '// a.href = x is how it used to read',
    ]) {
      expect(urlSinkLines('fixture.js', safe)).toEqual([]);
    }
  });

  test('every named site still exists, so the list cannot outlive its reason', () => {
    const stale = ALLOWED.filter(
      (a) => !PLUGINS.some((p) => p.file === a.file && p.source.split('\n').some((l) => l.trim() === a.line)),
    ).map((a) => `${a.file}: ${a.line}`);
    expect(stale).toEqual([]);
  });

  test('no plugin hands appendChild a value that may be a string', () => {
    // The shape CodeQL read as HTML at sign-off.js:84: a conditional
    // whose one arm is the raw child. `append(x instanceof Node ? x :
    // String(x))` is the replacement — a string there is a Text node.
    const found = PLUGINS.flatMap(({ file, source }) =>
      source
        .split('\n')
        .map((l) => l.trim())
        .filter((l) => /appendChild\([^()]*\?/.test(l))
        .map((l) => `${file}: ${l}`),
    );
    expect(found).toEqual([]);
  });
});
