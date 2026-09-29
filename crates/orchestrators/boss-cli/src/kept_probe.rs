//! Does a car's KEPT probe still read the head it now vouches for?
//!
//! WHY (backlog 79a17c7a). `boss rerail --finish` is the designed repair
//! for a BARE re-gate — one with no `--park-*` flags — and a bare re-gate
//! carries no probe, so the car keeps the one its FIRST gate stated. Car
//! bfb219b4 (2026-09-28) was parked with a probe grepping
//! `mktemp -p "$RUNTIME_DIRECTORY" forge-auth.XXXXXX`; the adversarial
//! review's fold changed that line to `mktemp -p "$rtdir"`, the new head
//! was re-gated bare and finished onto the car, and after landing the
//! forge recheck proved the car FAILING on correct code. Of the 11 cars
//! `--finish` refreshed on 2026-09-27/28, all 11 kept the first gate's
//! probe; 10 proved only because the old strings happened to survive.
//!
//! So `--finish` replays the kept probe's GREP NEEDLES — the literal
//! patterns it can read off the probe text, each with the file it greps —
//! against the head it copied, and WARNS naming each string that head no
//! longer holds. A warning, not a refusal: a bare re-gate is the right
//! repair for a car whose change did not touch what its probe reads, and
//! the probe is the builder's text, which this reads only as far as it can
//! be sure of. A needle it cannot read literally (a `$var`, a `-f` file, a
//! grep of an API answer rather than the tree) is left out, never guessed.
//!
//! "No longer" is judged against the head the probe was written for: a
//! needle absent from BOTH heads is a probe asserting absence, and says
//! nothing about the change.

use std::process::Command;

/// One string a probe greps the tree for, with how to replay it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Needle {
    /// The pattern, exactly as the probe's grep receives it.
    pub pattern: String,
    /// The `git grep` flags that make the replay match what the probe's
    /// grep would: the regex flavour (`-F`, `-E`, `-G`) and `-i`/`-w`/`-x`.
    pub flags: Vec<&'static str>,
    /// The tree paths the probe greps; empty means the whole tree.
    pub paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    /// A shell word, and whether it is LITERAL (no `$` or backtick in it).
    Word(String, bool),
    Pipe,
    /// Ends a pipeline: newline, `;`, `&`, `&&`, `||`, parentheses, `$(`.
    End,
    /// A redirection operator; the next word is its target, not an argument.
    Redir,
}

/// A small shell lexer: enough of POSIX quoting to recover a grep's
/// arguments exactly, and pipelines well enough to know what a grep reads.
fn lex(src: &str) -> Vec<Tok> {
    let mut out = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut literal = true;
    let mut chars = src.chars().peekable();
    let flush = |out: &mut Vec<Tok>, word: &mut String, in_word: &mut bool, literal: &mut bool| {
        if *in_word {
            out.push(Tok::Word(std::mem::take(word), *literal));
        }
        *in_word = false;
        *literal = true;
    };
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                in_word = true;
                for q in chars.by_ref() {
                    if q == '\'' {
                        break;
                    }
                    word.push(q);
                }
            }
            '"' => {
                in_word = true;
                while let Some(q) = chars.next() {
                    match q {
                        '"' => break,
                        '\\' => match chars.peek() {
                            Some('"' | '\\' | '$' | '`') => word.extend(chars.next()),
                            _ => word.push('\\'),
                        },
                        '$' | '`' => {
                            literal = false;
                            word.push(q);
                        }
                        _ => word.push(q),
                    }
                }
            }
            '\\' => {
                in_word = true;
                match chars.next() {
                    Some('\n') | None => {}
                    Some(n) => word.push(n),
                }
            }
            '#' if !in_word => {
                for q in chars.by_ref() {
                    if q == '\n' {
                        break;
                    }
                }
                out.push(Tok::End);
            }
            ' ' | '\t' => flush(&mut out, &mut word, &mut in_word, &mut literal),
            // A brace group opens and closes a list; inside a word
            // (`${n:-empty}`) a brace is just text.
            '{' | '}' if !in_word => {
                flush(&mut out, &mut word, &mut in_word, &mut literal);
                out.push(Tok::End);
            }
            '\n' | ';' | '&' | '(' | ')' | '`' => {
                flush(&mut out, &mut word, &mut in_word, &mut literal);
                out.push(Tok::End);
            }
            '|' => {
                flush(&mut out, &mut word, &mut in_word, &mut literal);
                if chars.peek() == Some(&'|') {
                    chars.next();
                    out.push(Tok::End);
                } else {
                    out.push(Tok::Pipe);
                }
            }
            '>' | '<' => {
                // `2>` / `2>&1`: the fd number is part of the operator.
                if in_word
                    && literal
                    && !word.is_empty()
                    && word.chars().all(|d| d.is_ascii_digit())
                {
                    word.clear();
                    in_word = false;
                }
                flush(&mut out, &mut word, &mut in_word, &mut literal);
                while matches!(chars.peek(), Some('>' | '<' | '&')) {
                    chars.next();
                }
                out.push(Tok::Redir);
            }
            '$' if chars.peek() == Some(&'(') => {
                flush(&mut out, &mut word, &mut in_word, &mut literal);
                chars.next();
                out.push(Tok::End);
            }
            '$' => {
                in_word = true;
                literal = false;
                word.push('$');
            }
            _ => {
                in_word = true;
                word.push(c);
            }
        }
    }
    flush(&mut out, &mut word, &mut in_word, &mut literal);
    out
}

/// Pipelines, each a list of commands, each a list of `(word, literal)`.
fn pipelines(toks: Vec<Tok>) -> Vec<Vec<Vec<(String, bool)>>> {
    let mut all = Vec::new();
    let mut pipe: Vec<Vec<(String, bool)>> = vec![Vec::new()];
    let mut skip_next = false;
    for t in toks {
        match t {
            Tok::Word(w, lit) => {
                if skip_next {
                    skip_next = false;
                } else if let Some(cmd) = pipe.last_mut() {
                    cmd.push((w, lit));
                }
            }
            Tok::Redir => skip_next = true,
            Tok::Pipe => {
                skip_next = false;
                pipe.push(Vec::new());
            }
            Tok::End => {
                skip_next = false;
                if pipe.iter().any(|c| !c.is_empty()) {
                    all.push(std::mem::replace(&mut pipe, vec![Vec::new()]));
                } else {
                    pipe = vec![Vec::new()];
                }
            }
        }
    }
    if pipe.iter().any(|c| !c.is_empty()) {
        all.push(pipe);
    }
    all
}

/// The command's words past its shell keywords and assignments, and
/// whether a `!` negated it.
fn strip_prefix(cmd: &[(String, bool)]) -> (&[(String, bool)], bool) {
    let mut negated = false;
    let mut i = 0;
    while let Some((w, _)) = cmd.get(i) {
        let assignment = w.split_once('=').is_some_and(|(k, _)| {
            !k.is_empty() && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        });
        match w.as_str() {
            "!" => negated = true,
            "if" | "then" | "elif" | "else" | "while" | "until" | "do" | "time" => {}
            _ if assignment => {}
            _ => break,
        }
        i += 1;
    }
    (&cmd[i..], negated)
}

/// The tree path a pipeline's source command reads, if it reads one:
/// `git show <rev>:<path>` or `cat <path>`.
fn source_path(cmd: &[(String, bool)]) -> Option<String> {
    let (words, _) = strip_prefix(cmd);
    let w: Vec<&str> = words.iter().map(|(s, _)| s.as_str()).collect();
    let lit = words.iter().all(|(_, l)| *l);
    match w.as_slice() {
        ["git", "show", spec, ..] if lit => spec
            .split_once(':')
            .map(|(_, p)| p.to_string())
            .filter(|p| !p.is_empty()),
        ["cat", path] if lit => Some((*path).to_string()),
        _ => None,
    }
}

/// The needles one grep command states, or none when it cannot be read
/// literally. `stdin_path` is what the grep reads when it names no file.
fn grep_needles(cmd: &[(String, bool)], stdin_path: Option<&str>) -> Vec<Needle> {
    let (words, negated) = strip_prefix(cmd);
    if negated {
        return Vec::new();
    }
    let (git, rest, mut flavour) = match words.first().map(|(w, _)| w.as_str()) {
        Some("grep") => (false, &words[1..], "-G"),
        Some("egrep") => (false, &words[1..], "-E"),
        Some("fgrep") => (false, &words[1..], "-F"),
        Some("git") if words.get(1).map(|(w, _)| w.as_str()) == Some("grep") => {
            (true, &words[2..], "-G")
        }
        _ => return Vec::new(),
    };
    let mut extra: Vec<&'static str> = Vec::new();
    let mut patterns: Vec<(String, bool)> = Vec::new();
    let mut positional: Vec<(String, bool)> = Vec::new();
    let mut after_dashdash: Vec<(String, bool)> = Vec::new();
    let mut unreadable = false;
    let mut options_done = false;
    let mut seen_dashdash = false;
    let mut it = rest.iter();
    while let Some((w, lit)) = it.next() {
        if seen_dashdash {
            after_dashdash.push((w.clone(), *lit));
            continue;
        }
        if w == "--" {
            if git {
                seen_dashdash = true;
            } else {
                options_done = true;
            }
            continue;
        }
        if options_done || !w.starts_with('-') || w == "-" {
            positional.push((w.clone(), *lit));
            continue;
        }
        if let Some(long) = w.strip_prefix("--") {
            let (name, val) = match long.split_once('=') {
                Some((n, v)) => (n, Some(v.to_string())),
                None => (long, None),
            };
            match name {
                "fixed-strings" => flavour = "-F",
                "extended-regexp" => flavour = "-E",
                "basic-regexp" => flavour = "-G",
                "ignore-case" => extra.push("-i"),
                "word-regexp" => extra.push("-w"),
                "line-regexp" => extra.push("-x"),
                "invert-match" | "perl-regexp" | "file" => unreadable = true,
                "regexp" => match val {
                    Some(v) => patterns.push((v, *lit)),
                    None => patterns.extend(it.next().cloned()),
                },
                "max-count" | "after-context" | "before-context" | "context" if val.is_none() => {
                    it.next();
                }
                _ => {}
            }
            continue;
        }
        let cluster: Vec<char> = w[1..].chars().collect();
        for (i, c) in cluster.iter().enumerate() {
            match c {
                'F' => flavour = "-F",
                'E' => flavour = "-E",
                'G' => flavour = "-G",
                'i' => extra.push("-i"),
                'w' => extra.push("-w"),
                'x' => extra.push("-x"),
                'v' | 'P' | 'f' => unreadable = true,
                'e' | 'm' | 'A' | 'B' | 'C' | 'd' | 'D' => {
                    let attached: String = cluster[i + 1..].iter().collect();
                    let arg = if attached.is_empty() {
                        it.next().cloned()
                    } else {
                        Some((attached, *lit))
                    };
                    if *c == 'e' {
                        patterns.extend(arg);
                    }
                    break;
                }
                _ => {}
            }
        }
    }
    if unreadable {
        return Vec::new();
    }
    let mut operands = positional.into_iter();
    if patterns.is_empty() {
        patterns.extend(operands.next());
    }
    let operands: Vec<(String, bool)> = operands.collect();
    let paths: Vec<(String, bool)> = if git {
        // `git grep <pat> [<rev>...] [-- <path>...]`: before `--`, the
        // first operand is the revision the probe reads (HEAD).
        if seen_dashdash {
            after_dashdash
        } else {
            operands.into_iter().skip(1).collect()
        }
    } else if operands.is_empty() {
        match stdin_path {
            Some(p) => vec![(p.to_string(), true)],
            // Reads something that is not the tree — an API answer, a
            // variable — so there is nothing here to replay.
            None => return Vec::new(),
        }
    } else {
        operands
    };
    if paths.iter().any(|(_, lit)| !lit) {
        return Vec::new();
    }
    let paths: Vec<String> = paths.into_iter().map(|(p, _)| p).collect();
    let mut flags = vec![flavour];
    flags.extend(extra);
    patterns
        .into_iter()
        .filter(|(p, lit)| *lit && !p.is_empty())
        .map(|(pattern, _)| Needle {
            pattern,
            flags: flags.clone(),
            paths: paths.clone(),
        })
        .collect()
}

/// PURE: every tree needle a probe greps for, in the order it greps them.
pub(crate) fn needles(probe: &str) -> Vec<Needle> {
    let mut out: Vec<Needle> = Vec::new();
    for pipe in pipelines(lex(probe)) {
        // `! a | grep x` negates the whole pipeline: a probe asserting
        // absence, which says nothing about a string going missing.
        if pipe.first().is_some_and(|c| strip_prefix(c).1) {
            continue;
        }
        let stdin_path = pipe.first().and_then(|c| source_path(c));
        for (i, cmd) in pipe.iter().enumerate() {
            let feed = if i == 0 { None } else { stdin_path.as_deref() };
            for n in grep_needles(cmd, feed) {
                if !out.contains(&n) {
                    out.push(n);
                }
            }
        }
    }
    out
}

/// Does `rev` hold a match for the needle? `None` when git cannot say
/// (an unknown revision, a bad pattern) — which is not an answer.
///
/// The probe's paths are from the top of the tree (it runs in the
/// converged checkout's root), while this may run in a subdirectory,
/// where git grep reads a bare path — and an empty pathspec — as
/// relative to it. So every path is anchored at the top, literally.
pub(crate) fn holds(dir: &str, rev: &str, n: &Needle) -> Option<bool> {
    let specs: Vec<String> = if n.paths.is_empty() {
        vec![":(top)".to_string()]
    } else {
        n.paths
            .iter()
            .map(|p| format!(":(top,literal){}", p.trim_start_matches("./")))
            .collect()
    };
    let (flags, pattern) = git_grep_form(n);
    let mut cmd = Command::new("git");
    cmd.args(["-C", dir, "grep", "-q"])
        .args(&flags)
        .args(["-e", &pattern, rev, "--"])
        .args(&specs);
    match cmd.output().ok()?.status.code() {
        Some(0) => Some(true),
        Some(1) => Some(false),
        _ => None,
    }
}

/// PURE: the needle as `git grep` takes it. git grep has no `-x` (it
/// refuses the switch, measured 2026-09-28 on car 25377383's `grep -cx`),
/// so a whole-line match is written as an anchored pattern instead — a
/// fixed string escaped into a basic regex first.
fn git_grep_form(n: &Needle) -> (Vec<&'static str>, String) {
    let flags: Vec<&'static str> = n.flags.iter().copied().filter(|f| *f != "-x").collect();
    if flags.len() == n.flags.len() {
        return (flags, n.pattern.clone());
    }
    let (flags, pattern) = match flags.first().copied() {
        Some("-E") => (flags, format!("^({})$", n.pattern)),
        Some("-F") => {
            let escaped: String = n
                .pattern
                .chars()
                .flat_map(|c| {
                    let esc = matches!(c, '\\' | '.' | '[' | ']' | '*' | '^' | '$');
                    esc.then_some('\\').into_iter().chain(std::iter::once(c))
                })
                .collect();
            let mut f = flags;
            f[0] = "-G";
            (f, format!("^{escaped}$"))
        }
        _ => (flags, format!("^\\({}\\)$", n.pattern)),
    };
    (flags, pattern)
}

/// PURE: the needles the new head no longer holds. A needle counts only
/// when the head the probe was written for held it (`was`), or when that
/// head could not be read and the new head plainly does not; a needle
/// neither head holds is a probe asserting absence.
pub(crate) fn gone(checked: &[(Needle, Option<bool>, Option<bool>)]) -> Vec<&Needle> {
    checked
        .iter()
        .filter(|(_, was, now)| *now == Some(false) && *was != Some(false))
        .map(|(n, _, _)| n)
        .collect()
}

/// PURE: what `--finish` says about a probe it kept. `None` when the car
/// has no probe to keep.
pub(crate) fn kept_probe_report(
    car: &str,
    probe: Option<&str>,
    written_for: Option<&str>,
    new_head: &str,
    checked: &[(Needle, Option<bool>, Option<bool>)],
) -> Option<String> {
    let probe = probe.filter(|p| !p.trim().is_empty())?;
    let short = |h: &str| h[..12.min(h.len())].to_string();
    let first_line = probe
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or(probe);
    let shown: String = first_line.chars().take(120).collect();
    let for_head = written_for
        .map(|h| format!("written for {}", short(h)))
        .unwrap_or_else(|| "written for the car's first head".to_string());
    let lost = gone(checked);
    let remedy = "to replace it, re-gate WITH --park-probe/--park-expect (or --park-file) \
                  — a green that carries park intent refreshes the car and its probe \
                  together — or finish from such a green";
    if !lost.is_empty() {
        let list: Vec<String> = lost
            .iter()
            .map(|n| {
                let at = if n.paths.is_empty() {
                    "the tree".to_string()
                } else {
                    n.paths.join(", ")
                };
                format!("    {:?} in {at}", n.pattern)
            })
            .collect();
        return Some(format!(
            "boss rerail: WARNING — car {car} KEEPS its probe ({for_head}), because the green \
             copied carries no park intent, and {} no longer holds {} string(s) it greps:\n\
             {}\n  probe: {shown}\n  A probe that greps a line the change rewrote proves the \
             car FAILING on correct code (car bfb219b4, backlog 79a17c7a); {remedy}.",
            short(new_head),
            lost.len(),
            list.join("\n"),
        ));
    }
    let judged = checked.iter().filter(|(_, _, now)| now.is_some()).count();
    Some(if judged == 0 {
        format!(
            "boss rerail: car {car} keeps its probe ({for_head}); it greps no tree string this \
             verb can check against {}, so read it before the car lands: {shown}",
            short(new_head)
        )
    } else {
        format!(
            "boss rerail: car {car} keeps its probe ({for_head}); {judged} string(s) it greps \
             are still on {}",
            short(new_head)
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn n(pattern: &str, flags: &[&'static str], paths: &[&str]) -> Needle {
        Needle {
            pattern: pattern.to_string(),
            flags: flags.to_vec(),
            paths: paths.iter().map(|p| p.to_string()).collect(),
        }
    }

    /// Car bfb219b4's own shape: a single-quoted pattern carrying double
    /// quotes, grepped out of `git show HEAD:<path>`.
    #[test]
    fn a_git_show_pipeline_names_its_file_and_its_literal_pattern() {
        let p = r#"n=$(git show HEAD:infra/forge/converge.sh | grep -c 'mktemp -p "$RUNTIME_DIRECTORY" forge-auth.XXXXXX')"#;
        assert_eq!(
            needles(p),
            vec![n(
                r#"mktemp -p "$RUNTIME_DIRECTORY" forge-auth.XXXXXX"#,
                &["-G"],
                &["infra/forge/converge.sh"]
            )]
        );
    }

    /// The shapes the recorded probes use (read off 150 live cars,
    /// 2026-09-28): several `-e`, `-F`/`-E` flavours, `git grep ... HEAD
    /// -- path`, a grep of a named file, and `|| { ... }` tails.
    #[test]
    fn the_recorded_probe_shapes_are_read() {
        let p = "p=$(git show HEAD:a/port.rs | grep -c -e '^pub fn x(' -e '^pub fn y(')";
        assert_eq!(
            needles(p),
            vec![
                n("^pub fn x(", &["-G"], &["a/port.rs"]),
                n("^pub fn y(", &["-G"], &["a/port.rs"])
            ]
        );
        let g = "put=$(git grep -c 'export async function putStep' HEAD -- \"libs/door.ts\" | cut -d: -f3)";
        assert_eq!(
            needles(g),
            vec![n(
                "export async function putStep",
                &["-G"],
                &["libs/door.ts"]
            )]
        );
        let f = "grep -qF 'a.b' infra/x.sh 2>/dev/null || { echo 'FAILED'; exit 1; }";
        assert_eq!(needles(f), vec![n("a.b", &["-F"], &["infra/x.sh"])]);
        let e = "git show HEAD:x.rs | grep -Eq 'fn (a|b)' && echo ok";
        assert_eq!(needles(e), vec![n("fn (a|b)", &["-E"], &["x.rs"])]);
        let m = "git show HEAD:x.rs | grep -m1 -i 'Park-Probe'";
        assert_eq!(needles(m), vec![n("Park-Probe", &["-G", "-i"], &["x.rs"])]);
    }

    /// Never guessed: a pattern or path in a variable, an inverted or
    /// negated grep, a grep of an API answer or of a captured variable.
    #[test]
    fn what_cannot_be_read_literally_is_left_out() {
        for p in [
            "git show HEAD:$f | grep -c 'x'",
            "git show HEAD:a.rs | grep -c \"$key\"",
            "git show HEAD:a.rs | grep -vc 'x'",
            "! git show HEAD:a.rs | grep -q 'x'",
            "boss-sor-read '/api/jobs' | grep -c 'x'",
            "printf '%s\\n' \"$f\" | grep -q '^fn x'",
            "git show HEAD:a.rs | grep -f patterns.txt",
            "# git show HEAD:a.rs | grep -q 'commented'",
        ] {
            assert_eq!(needles(p), vec![], "{p}");
        }
    }

    /// A needle is GONE only when the new head lacks it and the head the
    /// probe was written for did not also lack it (a probe asserting
    /// absence), and an unanswerable check is never counted either way.
    #[test]
    fn gone_means_held_before_and_missing_now() {
        let a = n("a", &["-G"], &[]);
        let checked = vec![
            (a.clone(), Some(true), Some(false)),
            (n("b", &["-G"], &[]), Some(false), Some(false)),
            (n("c", &["-G"], &[]), None, Some(false)),
            (n("d", &["-G"], &[]), Some(true), None),
            (n("e", &["-G"], &[]), Some(true), Some(true)),
        ];
        let g: Vec<&str> = gone(&checked).iter().map(|n| n.pattern.as_str()).collect();
        assert_eq!(g, ["a", "c"]);
    }

    /// THE bfb219b4 REPLAY, on a real repository: the first head holds
    /// the probed line, the fold rewrites it, and the report names the
    /// kept probe, the string it lost and the call that replaces it.
    #[test]
    fn a_kept_probe_whose_line_the_fold_rewrote_is_warned_about_by_name() {
        let repo = boss_testing::scratch_dir("kept-probe-fold");
        let r = repo.to_str().expect("utf-8").to_string();
        let git = |args: &[&str]| {
            let o = Command::new("git")
                .args(["-C", &r, "-c", "user.email=t@t", "-c", "user.name=t"])
                .args(args)
                .output()
                .expect("git");
            assert!(o.status.success(), "{args:?}: {o:?}");
            String::from_utf8_lossy(&o.stdout).trim().to_string()
        };
        git(&["init", "-q", "-b", "main"]);
        std::fs::create_dir_all(Path::new(&r).join("infra/forge")).expect("mkdir");
        let file = Path::new(&r).join("infra/forge/converge.sh");
        std::fs::write(
            &file,
            "t=$(mktemp -p \"$RUNTIME_DIRECTORY\" forge-auth.XXXXXX)\nkeep\n",
        )
        .expect("write");
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "first draft"]);
        let first = git(&["rev-parse", "HEAD"]);
        std::fs::write(&file, "t=$(mktemp -p \"$rtdir\" forge-auth.XXXXXX)\nkeep\n")
            .expect("write");
        git(&["commit", "-q", "-am", "the review's fold"]);
        let folded = git(&["rev-parse", "HEAD"]);

        let probe = "n=$(git show HEAD:infra/forge/converge.sh | grep -c 'mktemp -p \"$RUNTIME_DIRECTORY\" forge-auth.XXXXXX')\n\
                     k=$(git show HEAD:infra/forge/converge.sh | grep -c '^keep$')";
        let checked: Vec<_> = needles(probe)
            .into_iter()
            .map(|nd| {
                let was = holds(&r, &first, &nd);
                let now = holds(&r, &folded, &nd);
                (nd, was, now)
            })
            .collect();
        assert_eq!(checked.len(), 2, "{checked:?}");
        let w = kept_probe_report("bfb219b4", Some(probe), Some(&first), &folded, &checked)
            .expect("a kept probe is reported");
        assert!(w.contains("WARNING"), "{w}");
        assert!(w.contains("car bfb219b4 KEEPS its probe"), "{w}");
        assert!(
            w.contains(&first[..12]),
            "names the head it was written for: {w}"
        );
        assert!(
            w.contains(&folded[..12]),
            "names the head it no longer reads: {w}"
        );
        assert!(
            w.contains("forge-auth.XXXXXX"),
            "names the lost string: {w}"
        );
        assert!(
            !w.contains("^keep$"),
            "a string still held is not named: {w}"
        );
        assert!(w.contains("--park-probe"), "names the repair: {w}");

        // A fold that left the probed lines alone is said, not warned.
        let ok = kept_probe_report(
            "bfb219b4",
            Some(probe),
            Some(&first),
            &first,
            &[(
                n("^keep$", &["-G"], &["infra/forge/converge.sh"]),
                Some(true),
                Some(true),
            )],
        )
        .expect("reported");
        assert!(!ok.contains("WARNING"), "{ok}");
        assert!(ok.contains("1 string(s) it greps are still on"), "{ok}");

        // A car with no probe has nothing to keep.
        assert_eq!(kept_probe_report("x", None, None, &folded, &[]), None);
    }

    /// A probe that greps nothing checkable still says the car keeps it,
    /// and shows it — a kept probe is never silent (79a17c7a).
    #[test]
    fn a_kept_probe_with_no_tree_string_is_still_named() {
        let w = kept_probe_report(
            "6c6acd83",
            Some("boss-sor-read '/api/x' | jq -r .total"),
            Some("0123456789abcdef"),
            "fedcba9876543210",
            &[],
        )
        .expect("reported");
        assert!(
            w.contains("keeps its probe (written for 0123456789ab)"),
            "{w}"
        );
        assert!(w.contains("boss-sor-read '/api/x'"), "{w}");
    }

    /// A whole-line grep replays as one: git grep refuses `-x`, which
    /// read every `grep -cx` probe as unanswerable (car 25377383's shape).
    #[test]
    fn a_whole_line_grep_is_replayed_as_an_anchored_match() {
        let repo = boss_testing::scratch_dir("kept-probe-whole-line");
        let r = repo.to_str().expect("utf-8").to_string();
        let git = |args: &[&str]| {
            let o = Command::new("git")
                .args(["-C", &r, "-c", "user.email=t@t", "-c", "user.name=t"])
                .args(args)
                .output()
                .expect("git");
            assert!(o.status.success(), "{args:?}: {o:?}");
        };
        git(&["init", "-q", "-b", "main"]);
        std::fs::write(
            Path::new(&r).join("roles.toml"),
            "[roles.ml-batch-host]\nkeep it\n",
        )
        .expect("write");
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "c"]);
        let probe = "git show HEAD:roles.toml | grep -cx '\\[roles\\.ml-batch-host\\]'\n\
                     git show HEAD:roles.toml | grep -cx 'keep'\n\
                     git show HEAD:roles.toml | grep -cxF 'keep.it'\n\
                     git show HEAD:roles.toml | grep -cxF 'keep it'\n\
                     git show HEAD:roles.toml | grep -cxE 'keep (it|that)'";
        let got: Vec<Option<bool>> = needles(probe)
            .iter()
            .map(|n| holds(&r, "HEAD", n))
            .collect();
        assert_eq!(
            got,
            [Some(true), Some(false), Some(false), Some(true), Some(true)]
        );
    }
}
