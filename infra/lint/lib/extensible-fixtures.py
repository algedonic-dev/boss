"""Find exhaustive fixture expressions, not comments, types or patterns.

Local declarations and imports are resolved within their lexical scopes.
Unknown qualified domain paths, ambiguous globs and conflicting bindings
refuse with a named diagnostic. Balanced delimiters ensure a nested range
or update cannot excuse an exhaustive outer expression (caa2acc9 D2).
"""
import json
import re
import sys
from pathlib import Path


def tokens(source):
    result = []
    i = 0
    while i < len(source):
        if source[i].isspace():
            i += 1
            continue
        if source.startswith('//', i):
            end = source.find('\n', i)
            i = len(source) if end < 0 else end
            continue
        if source.startswith('/*', i):
            depth = 1
            i += 2
            while depth and i < len(source):
                if source.startswith('/*', i):
                    depth += 1
                    i += 2
                elif source.startswith('*/', i):
                    depth -= 1
                    i += 2
                else:
                    i += 1
            if depth:
                raise ValueError('unterminated block comment')
            continue
        raw = re.match(r'(?:br|cr|r)(#*)"', source[i:])
        if raw:
            end_marker = '"' + raw[1]
            end = source.find(end_marker, i + raw.end())
            if end < 0:
                raise ValueError('unterminated raw string')
            result.append(('literal', i, end + len(end_marker)))
            i = end + len(end_marker)
            continue
        quoted = re.match(r'(?:b|c)?"(?:\\.|[^"\\])*"|(?:b)?\'(?:\\.|[^\'\\])\'', source[i:], re.S)
        if quoted:
            result.append(('literal', i, i + quoted.end()))
            i += quoted.end()
            continue
        token = re.match(r'r#[A-Za-z_][A-Za-z_0-9]*|[A-Za-z_][A-Za-z_0-9]*|::|\.\.=|\.\.|=>|->|[^\s]', source[i:])
        value = token[0][2:] if token[0].startswith('r#') else token[0]
        result.append((value, i, i + token.end()))
        i += token.end()
    return result


def literals(source):
    ts = tokens(source)
    # A declaration shadows names only inside its own lexical scope.
    # Rust items are visible throughout that scope, including before the
    # item itself, so collect bindings before judging expressions.
    scopes = [{'parent': None, 'bindings': {}, 'modules': {}, 'unknown_globs': []}]
    token_scopes = []
    opened_scopes = {}
    current = 0
    for value, _, _ in ts:
        token_scopes.append(current)
        if value == '{':
            scopes.append({'parent': current, 'bindings': {}, 'modules': {}, 'unknown_globs': []})
            opened_scopes[len(token_scopes) - 1] = len(scopes) - 1
            current = len(scopes) - 1
        elif value == '}':
            parent = scopes[current]['parent']
            if parent is None:
                raise ValueError('unbalanced scope')
            current = parent
    if current != 0:
        raise ValueError('unterminated scope')
    canonical = {name: name for name in ('Job', 'Step', 'StepField')}
    module_scopes = {0}
    def bind(scope, name, value, position):
        bindings = scopes[scope]['bindings']
        if name in bindings and bindings[name] != value:
            raise ValueError(f"line {source.count(chr(10), 0, position) + 1}: conflicting binding for {name}")
        bindings[name] = value

    for i in range(len(ts) - 2):
        if ts[i][0] in ('struct', 'enum', 'union'):
            bind(token_scopes[i], ts[i + 1][0], None, ts[i][1])
        elif ts[i][0] == 'mod' and ts[i + 2][0] == '{':
            scopes[token_scopes[i]]['modules'][ts[i + 1][0]] = opened_scopes[i + 2]
            module_scopes.add(opened_scopes[i + 2])

    def module(name, scope):
        while scope is not None:
            if name in scopes[scope]['modules']:
                return scopes[scope]['modules'][name]
            scope = scopes[scope]['parent']
        return None

    def path_scope(path, scope):
        if not path:
            return scope
        if path[0] == 'crate':
            current = 0
            path = path[1:]
        elif path[0] == 'self':
            current = scope
            while current not in module_scopes:
                current = scopes[current]['parent']
            path = path[1:]
        elif path[0] == 'super':
            current = scope
            while current not in module_scopes:
                current = scopes[current]['parent']
            current = scopes[current]['parent']
            while current is not None and current not in module_scopes:
                current = scopes[current]['parent']
            path = path[1:]
        else:
            current = module(path[0], scope)
            path = path[1:]
        for name in path:
            if current is None:
                return None
            current = scopes[current]['modules'].get(name)
        return current

    def resolve_path(path, scope, seen):
        if len(path) >= 2 and path[0] == 'boss_core' and path[:-1] in (('boss_core',), ('boss_core', 'job')):
            return canonical.get(path[-1])
        if len(path) == 1:
            return resolve(path[0], scope, seen)
        target = path_scope(path[:-1], scope)
        if target is not None:
            return resolve(path[-1], target, seen, implicit=False)
        if path[-1] in canonical:
            raise ValueError(f"unresolved qualified domain type {'::'.join(path)}")
        return None

    def imports(parts, prefix=()):
        values = [part[0] for part in parts]
        if '{' in values:
            opener = values.index('{')
            if not values or values[-1] != '}':
                raise ValueError('unsupported use group')
            head = tuple(value for value in values[:opener] if value != '::')
            start = opener + 1
            depth = 0
            for stop in range(start, len(values)):
                value = values[stop]
                if value == '{':
                    depth += 1
                elif value == '}':
                    depth -= 1
                if (value == ',' and depth == 0) or stop == len(values) - 1:
                    if start < stop:
                        yield from imports(parts[start:stop], prefix + head)
                    start = stop + 1
            return
        if 'as' in values:
            index = values.index('as')
            path = prefix + tuple(value for value in values[:index] if value != '::')
            if index + 2 != len(values):
                raise ValueError('unsupported use rename')
            yield values[-1], path
        else:
            path = prefix + tuple(value for value in values if value != '::')
            if not path:
                raise ValueError('empty use path')
            yield path[-1], path

    pending_globs = []
    for i, (value, position, _) in enumerate(ts):
        if value == 'type' and i + 2 < len(ts) and ts[i + 2][0] == '=':
            stop = i + 3
            while stop < len(ts) and ts[stop][0] != ';':
                stop += 1
            path = tuple(token[0] for token in ts[i + 3:stop] if token[0] != '::')
            bind(token_scopes[i], ts[i + 1][0], (path, token_scopes[i]), position)
        if value != 'use':
            continue
        stop = i + 1
        while stop < len(ts) and ts[stop][0] != ';':
            stop += 1
        if stop == len(ts):
            raise ValueError('unterminated use')
        scope = token_scopes[i]
        for name, path in imports(ts[i + 1:stop]):
            if name == '*':
                pending_globs.append((scope, path[:-1], position))
            else:
                bind(scope, name, (path, scope), position)
    for scope, path, position in pending_globs:
        if path in (('boss_core',), ('boss_core', 'job')):
            for name in canonical:
                bind(scope, name, ((path + (name,)), scope), position)
        else:
            target = path_scope(path, scope)
            if target is None:
                scopes[scope]['unknown_globs'].append(path)
                continue
            for name in scopes[target]['bindings']:
                bind(scope, name, (path + (name,), scope), position)

    def resolve(name, scope, seen=frozenset(), implicit=True):
        unknown_globs = []
        while scope is not None:
            bindings = scopes[scope]['bindings']
            unknown_globs.extend(scopes[scope]['unknown_globs'])
            if name in bindings:
                binding = bindings[name]
                if binding is None:
                    return None
                key = (scope, name)
                if key in seen:
                    raise ValueError(f'cyclic binding for {name}')
                path, origin = binding
                return resolve_path(path, origin, seen | {key})
            scope = scopes[scope]['parent']
            if not implicit:
                break
        if name in canonical and unknown_globs:
            raise ValueError(f"unresolved {name} under use glob {'::'.join(unknown_globs[0])}::*")
        return canonical.get(name) if implicit else None

    findings = []
    for n, (name, start, _) in enumerate(ts):
        if n + 1 == len(ts) or ts[n + 1][0] != '{':
            continue
        path_start = n
        while path_start >= 2 and ts[path_start - 1][0] == '::':
            path_start -= 2
        path = tuple(ts[index][0] for index in range(path_start, n + 1, 2))
        try:
            kind = resolve_path(path, token_scopes[n], frozenset())
        except ValueError as error:
            raise ValueError(f'line {source.count(chr(10), 0, start) + 1}: {error}') from error
        if kind is None:
            continue
        if path_start and ts[path_start - 1][0] in ('struct', 'enum', 'union', '->'):
            continue
        stack = ['{']
        fields = []
        segment = n + 2
        end = n + 2
        while end < len(ts) and stack:
            value = ts[end][0]
            if value in ('{', '(', '['):
                stack.append(value)
            elif value == '<' and (ts[end - 1][0] == '::' or stack[-1] == '<'):
                stack.append('<')
            elif value == '>' and stack[-1] == '<':
                stack.pop()
            elif value in ('}', ')', ']'):
                opener = {'}': '{', ')': '(', ']': '['}[value]
                if stack[-1] != opener:
                    raise ValueError('unbalanced delimiters')
                stack.pop()
                if not stack:
                    if segment < end:
                        fields.append(ts[segment:end])
                    break
            elif value == ',' and len(stack) == 1:
                if segment < end:
                    fields.append(ts[segment:end])
                segment = end + 1
            end += 1
        if stack:
            raise ValueError('unterminated candidate literal')
        if end + 1 < len(ts) and ts[end + 1][0] in ('=', '=>', ':', 'if'):
            continue  # let/match/parameter patterns
        # Attributes belong to the field, not its expression value.
        # Their delimiters were already balanced by the body scan.
        for index, field in enumerate(fields):
            while len(field) > 1 and field[0][0] == '#' and field[1][0] == '[':
                depth = 1
                stop = 2
                while stop < len(field) and depth:
                    if field[stop][0] == '[':
                        depth += 1
                    elif field[stop][0] == ']':
                        depth -= 1
                    stop += 1
                field = field[stop:]
            fields[index] = field
        valid = fields and all(
            f and (f[0][0] == '..' or
            (re.fullmatch(r'[A-Za-z_][A-Za-z_0-9]*', f[0][0]) and
             (len(f) == 1 or (len(f) > 2 and f[1][0] == ':'))))
            for f in fields
        )
        if not valid:
            continue
        findings.append({
            'kind': kind, 'start': start, 'end': ts[end][2],
            'line': source.count('\n', 0, start) + 1,
            'spread': any(f[0][0] == '..' for f in fields),
            'fields': [{'name': f[0][0], 'start': f[0][1], 'end': f[-1][2],
                        'value': source[f[2][1]:f[-1][2]] if len(f) > 2 and f[1][0] == ':' else f[0][0]}
                       for f in fields],
        })
    return findings


def self_test():
    controls = [
        ('Job { id: x, metadata: f(1..2) }', ['Job']),
        ('Step { fields: vec![StepField { name: x, ..base }], status: y }', ['Step']),
        ('Job { metadata: "..base", id: x }', ['Job']),
        ('Job { id: x, ..Job::new(a, b, c, d, e, f) }', []),
        ('// Job { id: x }\n/* nested /* Step { id: x } */ */', []),
        ('let s = r###"Job { id: x }"###; let c = b\'{\';', []),
        ('struct Job { id: X } fn f() -> Job { make() }', []),
        ('let Job { id: value } = input; match input { Step { id: x } => x }', []),
        ('fn f(Job { id: x }: Job) {}', []),
        ('boss_core::job::Job { id: x }', ['Job']),
        ('fn f() -> Job { Job { id: x } }', ['Job']),
        ('let x = StepField { name, field_type };', ['StepField']),
        ('struct Job { dir: PathBuf } fn f() { let j = Job { dir }; }', []),
        ('use boss_core::job::Job as Packet; fn f() { Packet { id: x } }', ['Job']),
        ('use boss_core::job::Job as Packet; mod helper { struct Packet { dir: PathBuf } } fn outer() { Packet { id: id, kind: kind } }', ['Job']),
        ('use boss_core::job::Job; mod helper { struct Job { dir: PathBuf } } fn outer() { Job { id: id, kind: kind } }', ['Job']),
        ('Job { #[cfg(test)] id: id, kind: make::<A, B>() }', ['Job']),
        ('match input { Job { id: value } if predicate(value) => value }', []),
        ('struct Job { dir: PathBuf } mod actual { use boss_core::job::Job; fn f() { Job { id: x, kind: y } } }', ['Job']),
        ('mod helper { struct Job { dir: PathBuf } } fn f() { helper::Job { dir: path } }', []),
        ('mod helper { struct Job { dir: PathBuf } } use helper::Job as Packet; fn f() { Packet { dir: path } }', []),
        ('mod actual { use boss_core::job::{Job as Packet}; fn f() { Packet { id: x } } }', ['Job']),
        ('fn f() { use boss_core::job::{Job as Packet}; Packet { id: x } }', ['Job']),
        ('use boss_core::job::Job as Packet; fn f() { struct Packet { dir: PathBuf } Packet { dir: x } } fn g() { Packet { id: x } }', ['Job']),
        ('type Packet = boss_core::job::Job; fn f() { Packet { id: x } }', ['Job']),
        ('mod actual { use boss_core::job::Job; fn f() { struct Job { dir: PathBuf } self::Job { id: x } } }', ['Job']),
        ('use boss_core::job::Job as Packet; mod actual { use super::Packet; fn f() { Packet { id: x } } }', ['Job']),
        ('struct r#Job { dir: PathBuf } fn f() { r#Job { dir: x } }', []),
    ]
    for source, expected in controls:
        actual = [f['kind'] for f in literals(source) if not f['spread']]
        if actual != expected:
            raise AssertionError((source, expected, actual))
    refusals = [
        ('use unknown::Job as Packet; fn f() { Packet { id: x } }', 'unresolved qualified domain type unknown::Job'),
        ('use boss_core::job::Job; struct Job { dir: PathBuf }', 'conflicting binding for Job'),
        ('use unknown::*; fn f() { Job { id: x } }', 'unresolved Job under use glob unknown::*'),
    ]
    for source, diagnostic in refusals:
        try:
            literals(source)
        except ValueError as error:
            if diagnostic not in str(error):
                raise AssertionError((source, diagnostic, str(error))) from error
        else:
            raise AssertionError((source, 'unsupported binding must refuse'))
    print(f'extensible-fixtures: {len(controls) + len(refusals)} lexical controls passed')


def main():
    if sys.argv[1:] == ['--self-test']:
        self_test()
        return 0
    root = Path(sys.argv[1]) if len(sys.argv) > 1 else Path(__file__).resolve().parents[3]
    files = sorted(p for p in (root / 'crates').rglob('*.rs') if 'tests' in p.relative_to(root / 'crates').parts and 'target' not in p.parts)
    if not files:
        raise ValueError('no integration Rust files scanned')
    findings = []
    for path in files:
        try:
            rows = literals(path.read_text())
        except ValueError as error:
            raise ValueError(f'{path.relative_to(root)}: {error}') from error
        for finding in rows:
            finding['file'] = str(path.relative_to(root))
            findings.append(finding)
    if '--inventory' in sys.argv:
        print(json.dumps(findings, indent=2))
        return 0
    failures = [f for f in findings if not f['spread']]
    for f in failures:
        print(f"{f['file']}:{f['line']}: exhaustive {f['kind']} fixture; use ..{f['kind']}::new(...) as its base", file=sys.stderr)
    print(f'test-fixtures-use-constructor-bases: scanned {len(files)} integration Rust files')
    print(f'test-fixtures-use-constructor-bases: {len(failures)} exhaustive fixtures')
    return 1 if failures else 0


if __name__ == '__main__':
    try:
        sys.exit(main())
    except (OSError, ValueError, AssertionError) as error:
        print(f'extensible-fixtures: refused: {error}', file=sys.stderr)
        sys.exit(1)
