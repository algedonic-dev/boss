//! DOES THIS PROBE READ NOTHING BUT GIT OBJECTS AT A NAMED REVISION — and
//! so mean the same thing in any checkout of this repository that holds
//! the car?
//!
//! THE DEFECT (backlog da937eb3, measured by protocol-retro adc3812b on
//! 2026-10-16): `boss prove --recheck` answered CANNOT RE-RUN HERE for 12
//! of 12 sampled W41 cars, recorded on `david-asus-minipc` (11) and a
//! dev-pod NAME that changes on every roll (1). Every sampled probe read
//! only `git show HEAD:<path>` or `git grep … HEAD`, whose answer is a
//! function of the objects at the ref, not of the host that holds them.
//!
//! A SMALL GRAMMAR, NOT A MODEL OF THE SHELL. Two reviews, two rounds of
//! holes. The first cut was a skip list, and review 2c318e8f found five
//! classes it let through (an assignment ahead of `git`, a globbed
//! program word, an attached `-Oorder`, the host's variables, a
//! redirection to a file). The rework modelled bash — variables a probe
//! assigned, `${x:-default}`, `case`, `for`, `$?` — and review 3b63b50f
//! (2026-10-16) found four more in the model itself: `$_` is set by the
//! shell, not the probe (`_=HEAD:a; echo --output=out.txt; git show
//! HEAD:a $_` handed git a write option); a `~` inside `${x:-~}` expands
//! to `$HOME`; a bare `set` prints the environment; and `printf
//! '%(…)T'` reads the clock. Each fix would be one more rule in a model
//! that has to be right about everything bash does. So the model is
//! gone. A word here has NO expansion at all: any `$`, backtick, glob,
//! brace or tilde prefix refuses, which means no variable, no
//! assignment and no `$(…)` — what a word says is what the command
//! receives. The one exception is a double-quoted `$?` in an `echo`
//! argument, the `|| { echo "… (exit $?)"; exit 1; }` report four
//! recorded probes use. On top of that, the only shapes are:
//!
//! - a program spelled exactly, unquoted: `git` (`show`, `log`,
//!   `cat-file`, `rev-parse`, `grep`), the stdin filters `grep`, `jq`,
//!   `wc`, `head`, `tail`, and `echo`, `printf` (a format whose every
//!   conversion is one of `diouxXeEfFgGaAcsbqQ`), `exit N`, `true`,
//!   `false`, `:` and `set` with at least one of `-e`, `-u`, `-x`,
//!   `-o pipefail` (28 of the 32 recorded probes this grammar reads open
//!   with one);
//! - an option that program is allowed, matched in full — an
//!   abbreviation (`--recurse`), an attached form (`-Oorder`) and every
//!   option nobody listed are refused alike;
//! - a revision spelled literally: `HEAD` or a full 40-digit object
//!   name, with `~N`/`^N`, a `..` range, or `REV:path`. Not a shorter
//!   hex name, which git resolves as a ref first when one of that name
//!   exists, and not `origin/<name>`: the witness in `boss prove` checks
//!   that the checkout's HEAD holds the car, and says nothing about its
//!   other refs. No relative date (`--since`, `--until`), which reads
//!   the clock;
//! - a connective: `|`, `&&`, `||`, `;`, a newline, `!` and `{ … }`;
//! - a redirection to `/dev/null` or of descriptor 1 or 2 to the other.
//!
//! Everything else refuses, including `if`, `case`, `for`, `while`,
//! `[`/`test`, a subshell, any `<`, a background `&`, a function and
//! every builtin not named above. THE COST, measured over the park
//! probes of the 497 newest closed cars: the shell model accepted 140,
//! this grammar accepts 32, and every one of the 108 it gave up reads a
//! value through `$` — 102 a `$(git show …)` capture, most of them then
//! tested by `case` or `[`. A refusal keeps the old recorded-host rule,
//! CANNOT RE-RUN HERE, which tests nothing; a false accept would run a
//! probe in a different checkout and could come back NO LONGER HOLDS
//! for a claim that still holds, or write in that checkout. Coverage is
//! the cheaper thing to lose, and a probe written as `git show
//! HEAD:<path> | grep -F '<line>' >/dev/null || { echo '…'; exit 75; }`
//! keeps it.

/// True only when `probe` invokes `git` and every word of it meets the
/// grammar above. [`git_objects_only`], answered yes or no.
pub fn reads_only_git_objects(probe: &str) -> bool {
    git_objects_only(probe).is_ok()
}

/// `Ok` only when `probe` invokes `git` and every word of it meets the
/// grammar above; otherwise the first place the text left it.
pub fn git_objects_only(probe: &str) -> Result<(), Refused> {
    let mut lexer = Lexer {
        chars: probe.chars().collect(),
        at: 0,
        start: 0,
    };
    let toks = lexer.tokens()?;
    let mut parser = Parser {
        toks: &toks,
        chars: &lexer.chars,
        at: 0,
        reads_git: false,
    };
    parser.list()?;
    if parser.at != toks.len() {
        return Err(parser.refused());
    }
    if parser.reads_git {
        Ok(())
    } else {
        Err(Refused::NoGit)
    }
}

/// WHERE THE TEXT LEFT THE GRAMMAR (backlog 70cb74d8). This was a unit
/// struct, on the reasoning that the door that asks prints the
/// recorded-host refusal, which already says why. It did not: three
/// reviews of car afd61aa2 (8d6976fa, da88bbff, 1042cf88) found that the
/// CANNOT RE-RUN HERE text could only list every cause it might have
/// been — this text, no checkout here holding the car, or no checkout
/// at all — and an operator rewriting a probe needs the one that applied
/// and the word it applied to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refused {
    /// The token that begins at character `at` (counted from 0) is a shell
    /// form outside the grammar: an expansion, a quote left open, a
    /// subshell, input, a redirection to a file, a background job, or a
    /// text that ends where a command must follow. `near` is the text
    /// from there to the end of its line, cut at 40 characters; empty at
    /// the end of the text.
    Shell { at: usize, near: String },
    /// A command whose program the grammar does not run, or whose
    /// arguments it does not allow — as written in the probe.
    Command { command: String },
    /// Every command fits, and none of them is `git`.
    NoGit,
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Shell { at, near } if near.is_empty() => write!(
                f,
                "it ends at character {}, where a command must follow",
                at + 1
            ),
            Self::Shell { at, near } => {
                write!(f, "it leaves the grammar at character {}: `{near}`", at + 1)
            }
            Self::Command { command } => write!(
                f,
                "the command `{command}` runs a program the grammar does not, or passes it \
                 an option it does not take"
            ),
            Self::NoGit => write!(
                f,
                "every command fits, but no `git` runs, so it reads no git object"
            ),
        }
    }
}

/// The longest excerpt a refusal quotes.
const NEAR: usize = 40;

/// The text from `from` to the end of its line, cut at [`NEAR`]
/// characters with a `…` when it was cut.
fn near(chars: &[char], from: usize) -> String {
    let line: Vec<char> = chars
        .iter()
        .skip(from)
        .take_while(|c| **c != '\n')
        .copied()
        .collect();
    let mut out: String = line.iter().take(NEAR).collect();
    if line.len() > NEAR {
        out.push('…');
    }
    out
}

type Fit<T> = Result<T, Refused>;

/// A word after quote removal — which, with no expansion allowed, is
/// exactly the argument the command receives. `plain` when no character
/// of it was quoted: only a plain word is a reserved word or a program.
#[derive(Debug)]
struct Word {
    text: String,
    plain: bool,
    /// Carries a double-quoted `$?`, so its text is not what the command
    /// receives: only `echo` may take it.
    status: bool,
    /// The character after its last, for a refusal that quotes it.
    end: usize,
}

#[derive(Debug)]
enum Tok {
    Word(Word),
    /// `|`, `||`, `&&`, `;` or a newline.
    Op(&'static str),
}

/// Bash's metacharacters: each ends a bare word.
const METACHARS: [char; 10] = [' ', '\t', '\n', '|', '&', ';', '(', ')', '<', '>'];

struct Lexer {
    chars: Vec<char>,
    at: usize,
    /// Where the token being read began: what a refusal points at.
    start: usize,
}

impl Lexer {
    fn refused(&self) -> Refused {
        Refused::Shell {
            at: self.start,
            near: near(&self.chars, self.start),
        }
    }

    fn fit(&self, ok: bool) -> Fit<()> {
        if ok { Ok(()) } else { Err(self.refused()) }
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.at).copied()
    }

    fn peek_at(&self, n: usize) -> Option<char> {
        self.chars.get(self.at + n).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek();
        self.at += 1;
        c
    }

    /// Every token of the text, each with the character it begins at.
    /// Redirections are checked here and dropped: only `/dev/null` and
    /// descriptor copies pass.
    fn tokens(&mut self) -> Fit<Vec<(usize, Tok)>> {
        let mut out = Vec::new();
        while let Some(c) = self.peek() {
            self.start = self.at;
            let at = self.at;
            match c {
                ' ' | '\t' => self.at += 1,
                '\\' if self.peek_at(1) == Some('\n') => self.at += 2,
                '\n' => {
                    self.at += 1;
                    out.push((at, Tok::Op("\n")));
                }
                '#' => {
                    while self.peek().is_some_and(|c| c != '\n') {
                        self.at += 1;
                    }
                }
                ';' => {
                    self.at += 1;
                    // `;;` belongs to `case`, which is not in the grammar.
                    self.fit(self.peek() != Some(';'))?;
                    out.push((at, Tok::Op(";")));
                }
                '|' => {
                    self.at += 1;
                    match self.peek() {
                        Some('|') => {
                            self.at += 1;
                            out.push((at, Tok::Op("||")));
                        }
                        Some('&') => return Err(self.refused()),
                        _ => out.push((at, Tok::Op("|"))),
                    }
                }
                '&' => {
                    self.at += 1;
                    match self.peek() {
                        Some('&') => {
                            self.at += 1;
                            out.push((at, Tok::Op("&&")));
                        }
                        Some('>') => self.redirection(None, true)?,
                        // A background job outlives the answer it feeds.
                        _ => return Err(self.refused()),
                    }
                }
                // A subshell, arithmetic, a function definition; input
                // from a file, a heredoc, a process substitution.
                '(' | ')' | '<' => return Err(self.refused()),
                '>' => self.redirection(None, false)?,
                _ => {
                    let word = self.word()?;
                    let fd = word.plain
                        && !word.text.is_empty()
                        && word.text.chars().all(|c| c.is_ascii_digit());
                    if fd && self.peek() == Some('>') {
                        self.redirection(Some(word.text), false)?;
                    } else {
                        out.push((at, Tok::Word(word)));
                    }
                }
            }
        }
        Ok(out)
    }

    /// `[fd]>`, `&>` or `>&N`, at the `>`. Output goes to `/dev/null` or
    /// to descriptor 1 or 2, never to a file: a probe run in another
    /// checkout must not write in it.
    fn redirection(&mut self, fd: Option<String>, both: bool) -> Fit<()> {
        self.fit(fd.as_deref().is_none_or(|fd| fd == "1" || fd == "2"))?;
        self.at += 1;
        let append = self.peek() == Some('>');
        if append {
            self.at += 1;
        }
        match self.peek() {
            Some('|') => Err(self.refused()),
            Some('&') => {
                self.at += 1;
                let target = self.bump();
                self.fit(
                    !both
                        && !append
                        && matches!(target, Some('1' | '2'))
                        && self.peek().is_none_or(|c| METACHARS.contains(&c)),
                )
            }
            _ => {
                while matches!(self.peek(), Some(' ' | '\t')) {
                    self.at += 1;
                }
                let target = self.word()?;
                self.fit(target.plain && target.text == "/dev/null")
            }
        }
    }

    /// One word, with its quotes removed. Every character that would make
    /// the shell expand it refuses: `$` (a variable, `$(…)`, `$?`,
    /// `$'…'`, arithmetic), a backtick, a glob, a brace, and a `~` that
    /// could begin a tilde prefix.
    fn word(&mut self) -> Fit<Word> {
        let mut text = String::new();
        let mut plain = true;
        let mut status = false;
        while let Some(c) = self.peek() {
            if METACHARS.contains(&c) {
                break;
            }
            self.at += 1;
            match c {
                '\\' => match self.bump() {
                    Some('\n') => {}
                    Some(e) => {
                        plain = false;
                        text.push(e);
                    }
                    None => return Err(self.refused()),
                },
                '\'' => {
                    plain = false;
                    loop {
                        match self.bump() {
                            Some('\'') => break,
                            Some(c) => text.push(c),
                            None => return Err(self.refused()),
                        }
                    }
                }
                '"' => {
                    plain = false;
                    status |= self.double_quoted(&mut text)?;
                }
                '$' | '`' => return Err(self.refused()),
                // `[` alone is the test program, `{` and `}` alone the
                // group's keywords; anywhere else each is a glob or a
                // brace expansion.
                '*' | '?' | '[' | '{' | '}' => self.fit(
                    c != '*'
                        && c != '?'
                        && plain
                        && text.is_empty()
                        && self.peek().is_none_or(|n| METACHARS.contains(&n)),
                )?,
                // A tilde prefix starts a word or follows `=` or `:`;
                // inside a revision (`HEAD~1`) it follows a letter, a
                // digit, `^` or `~`.
                '~' => self.fit(
                    text.chars()
                        .last()
                        .is_some_and(|p| p.is_ascii_alphanumeric() || p == '^' || p == '~'),
                )?,
                _ => {}
            }
            if !matches!(c, '\\' | '\'' | '"') {
                text.push(c);
            }
        }
        Ok(Word {
            text,
            plain,
            status,
            end: self.at,
        })
    }

    /// After an opening `"`, up to and including its close; true when it
    /// held a `$?`. No other `$` and no backtick, escaped or not. `$?` is
    /// the one expansion kept, because four recorded probes of 497 report
    /// a failed read as `echo "… (exit $?)"`: it is a number the shell
    /// sets from the command before it, and quoted it is one word.
    fn double_quoted(&mut self, text: &mut String) -> Fit<bool> {
        let mut status = false;
        loop {
            match self.bump() {
                Some('$') if self.peek() == Some('?') => {
                    self.at += 1;
                    status = true;
                    text.push_str("$?");
                }
                None | Some('$' | '`') => return Err(self.refused()),
                Some('"') => return Ok(status),
                Some('\\') => match self.bump() {
                    Some(e @ ('"' | '\\')) => text.push(e),
                    Some('\n') => {}
                    None | Some('$' | '`') => return Err(self.refused()),
                    Some(e) => {
                        text.push('\\');
                        text.push(e);
                    }
                },
                Some(c) => text.push(c),
            }
        }
    }
}

struct Parser<'t> {
    toks: &'t [(usize, Tok)],
    /// The probe's text, which a refusal quotes.
    chars: &'t [char],
    at: usize,
    reads_git: bool,
}

impl<'t> Parser<'t> {
    /// Refused at the token the parser stands on, or at the end of the
    /// text when it has run out of tokens.
    fn refused(&self) -> Refused {
        let at = self
            .toks
            .get(self.at)
            .map_or(self.chars.len(), |(start, _)| *start);
        Refused::Shell {
            at,
            near: near(self.chars, at),
        }
    }

    fn fit(&self, ok: bool) -> Fit<()> {
        if ok { Ok(()) } else { Err(self.refused()) }
    }

    fn peek(&self) -> Option<&'t Tok> {
        self.toks.get(self.at).map(|(_, tok)| tok)
    }

    fn peek_keyword(&self) -> Option<&'t str> {
        match self.peek() {
            Some(Tok::Word(w)) if w.plain => Some(&w.text),
            _ => None,
        }
    }

    fn expect_keyword(&mut self, kw: &str) -> Fit<()> {
        self.fit(self.peek_keyword() == Some(kw))?;
        self.at += 1;
        Ok(())
    }

    fn skip_newlines(&mut self) {
        while matches!(self.peek(), Some(Tok::Op("\n"))) {
            self.at += 1;
        }
    }

    /// Commands up to the end or a group's closing `}`.
    fn list(&mut self) -> Fit<()> {
        loop {
            while matches!(self.peek(), Some(Tok::Op(";" | "\n"))) {
                self.at += 1;
            }
            if self.peek().is_none() || self.peek_keyword() == Some("}") {
                return Ok(());
            }
            self.and_or()?;
            self.fit(matches!(self.peek(), None | Some(Tok::Op(";" | "\n"))))?;
        }
    }

    fn and_or(&mut self) -> Fit<()> {
        self.pipeline()?;
        while matches!(self.peek(), Some(Tok::Op("&&" | "||"))) {
            self.at += 1;
            self.skip_newlines();
            self.pipeline()?;
        }
        Ok(())
    }

    fn pipeline(&mut self) -> Fit<()> {
        if self.peek_keyword() == Some("!") {
            self.at += 1;
        }
        loop {
            self.command()?;
            if !matches!(self.peek(), Some(Tok::Op("|"))) {
                return Ok(());
            }
            self.at += 1;
            self.skip_newlines();
        }
    }

    /// A `{ … }` group or a simple command. No `if`, `case` or loop: of
    /// the 497 newest closed cars' probes, none that this grammar could
    /// otherwise read needed one.
    fn command(&mut self) -> Fit<()> {
        if self.peek_keyword() == Some("{") {
            self.at += 1;
            self.list()?;
            self.expect_keyword("}")
        } else {
            self.simple()
        }
    }

    fn simple(&mut self) -> Fit<()> {
        let first = self.at;
        let mut words = Vec::new();
        while let Some(Tok::Word(w)) = self.peek() {
            words.push(w);
            self.at += 1;
        }
        let Some((program, args)) = words.split_first() else {
            return Err(self.refused());
        };
        let fits = program.plain
            && (program.text == "echo" || !args.iter().any(|w| w.status))
            && self.fits(&program.text, args);
        if fits {
            return Ok(());
        }
        // The command as the probe spells it, from its first word to the
        // end of its last.
        let end = words.last().map_or(self.chars.len(), |w| w.end);
        Err(Refused::Command {
            command: spelled(self.chars, self.toks[first].0, end),
        })
    }

    /// Whether `program` takes these arguments.
    fn fits(&mut self, program: &str, args: &[&Word]) -> bool {
        let args: Vec<&str> = args.iter().map(|w| w.text.as_str()).collect();
        match program {
            "echo" => true,
            // `%(…)T` is the host's clock in the host's zone.
            "printf" => args
                .first()
                .is_some_and(|f| !f.starts_with('-') && !reads_the_clock(f)),
            "exit" => matches!(args.as_slice(), [] | [_]) && args.iter().all(|n| digits(n)),
            "true" | "false" | ":" => args.is_empty(),
            "set" => set_args(&args),
            "grep" => grep_args(&args),
            "jq" => jq_args(&args),
            "wc" => args.iter().all(|a| {
                matches!(
                    *a,
                    "-l" | "-c" | "-w" | "-m" | "--lines" | "--bytes" | "--words" | "--chars"
                )
            }),
            "head" | "tail" => head_args(&args, program == "tail"),
            "git" => {
                self.reads_git = true;
                git_args(&args)
            }
            _ => false,
        }
    }
}

/// The probe's text from `from` to `to`, as one line: cut at a newline
/// (a `\` continuation) or at twice [`NEAR`] characters, with a `…`.
fn spelled(chars: &[char], from: usize, to: usize) -> String {
    let all = &chars[from.min(to)..to.min(chars.len())];
    let line: String = all
        .iter()
        .take_while(|c| **c != '\n')
        .take(2 * NEAR)
        .collect();
    if line.chars().count() < all.len() {
        format!("{}…", line.trim_end_matches(['\\', ' ']))
    } else {
        line
    }
}

/// Whether a printf format holds a conversion other than a known pure
/// one, which is how `%(…)T` (the host's clock in the host's zone) is
/// refused in every spelling bash takes. An ALLOW-LIST, not a hunt for
/// `(`: after `%` bash skips flags (`-+ #0'`), a width and a precision
/// (digits or `*`) AND any run of length modifiers (`hjlLtz`) before
/// the conversion letter, so `%10(`, `%l(` and `%'5(` all read the
/// clock (reviews 8d6976fa and da88bbff, measured in bash 5.2). Only a
/// conversion letter that cannot read the clock passes; `%%` is a
/// literal percent, and a format ending in a bare `%` passes nothing.
fn reads_the_clock(format: &str) -> bool {
    let mut chars = format.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '%' {
            continue;
        }
        if chars.peek() == Some(&'%') {
            chars.next();
            continue;
        }
        while chars.next_if(|c| "-+ #0'".contains(*c)).is_some() {}
        while chars.next_if(|c| c.is_ascii_digit() || *c == '*').is_some() {}
        if chars.next_if_eq(&'.').is_some() {
            while chars.next_if(|c| c.is_ascii_digit() || *c == '*').is_some() {}
        }
        while chars.next_if(|c| "hjlLtz".contains(*c)).is_some() {}
        match chars.next() {
            Some(c) if "diouxXeEfFgGaAcsbqQ".contains(c) => {}
            _ => return true,
        }
    }
    false
}

fn digits(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_digit())
}

/// `set -e`, `-u`, `-x`, `-o pipefail`, and clusters of them — at least
/// one: a bare `set` prints every variable the host holds.
fn set_args(args: &[&str]) -> bool {
    let mut it = args.iter();
    let mut any = false;
    while let Some(s) = it.next() {
        let flags = s.strip_prefix(['-', '+']).unwrap_or("");
        // Each `o` takes the next word, and `set -o` with none lists the
        // shell's options: one per cluster, and it must name pipefail.
        let options = flags.matches('o').count();
        if flags.is_empty() || !flags.chars().all(|c| "euxo".contains(c)) || options > 1 {
            return false;
        }
        if options == 1 && it.next() != Some(&"pipefail") {
            return false;
        }
        any = true;
    }
    any
}

/// `-n N`, `-nN`, `-N`, `-c N`, `-cN` — and for `tail`, `+N`. No file,
/// no `-f`.
fn head_args(args: &[&str], tail: bool) -> bool {
    let count = |s: &str| {
        digits(if tail {
            s.strip_prefix('+').unwrap_or(s)
        } else {
            s
        })
    };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let ok = match *a {
            "-n" | "-c" => it.next().is_some_and(|n| count(n)),
            s => s
                .strip_prefix("-n")
                .or_else(|| s.strip_prefix("-c"))
                .or_else(|| s.strip_prefix('-'))
                .is_some_and(count),
        };
        if !ok {
            return false;
        }
    }
    true
}

/// `grep` filtering its stdin: one pattern (or `-e` patterns only), and
/// only options that shape the match. No file, no directory, no
/// `-f FILE`, and no long option but the ones spelled here in full.
fn grep_args(args: &[&str]) -> bool {
    const LONG: [&str; 17] = [
        "--quiet",
        "--silent",
        "--count",
        "--fixed-strings",
        "--extended-regexp",
        "--basic-regexp",
        "--perl-regexp",
        "--invert-match",
        "--ignore-case",
        "--line-number",
        "--line-regexp",
        "--word-regexp",
        "--only-matching",
        "--no-messages",
        "--text",
        "--no-filename",
        "--with-filename",
    ];
    const VALUED: [&str; 4] = [
        "--max-count=",
        "--after-context=",
        "--before-context=",
        "--context=",
    ];
    let (mut patterns, mut positionals, mut dashes) = (false, 0, false);
    let mut i = 0;
    while i < args.len() {
        let s = args[i];
        i += 1;
        if dashes {
            positionals += 1;
            continue;
        }
        if s == "--" {
            dashes = true;
        } else if s.starts_with("--") {
            if s.starts_with("--regexp=") {
                patterns = true;
            } else if !LONG.contains(&s)
                && !VALUED.iter().any(|v| s.strip_prefix(v).is_some_and(digits))
            {
                return false;
            }
        } else if s.len() > 1 && s.starts_with('-') {
            for (at, c) in s.char_indices().skip(1) {
                let rest = &s[at + c.len_utf8()..];
                if "qscvinxwEFGoPhHa".contains(c) {
                    continue;
                }
                if "mABC".contains(c) {
                    let ok = if rest.is_empty() {
                        i += 1;
                        args.get(i - 1).is_some_and(|n| digits(n))
                    } else {
                        digits(rest)
                    };
                    if !ok {
                        return false;
                    }
                    break;
                }
                if c == 'e' {
                    if rest.is_empty() {
                        i += 1;
                        if i > args.len() {
                            return false;
                        }
                    }
                    patterns = true;
                    break;
                }
                return false;
            }
        } else if !s.starts_with('-') {
            positionals += 1;
        } else {
            return false;
        }
    }
    positionals == usize::from(!patterns)
}

/// `jq` filtering its stdin: one program, options that shape output,
/// `--arg`/`--argjson`. No file argument, no `-f`, no `-L`, and no
/// program that reads the host — its environment (`env`, `$ENV`), a
/// module (`import`, `include`), its own location or the clock.
fn jq_args(args: &[&str]) -> bool {
    const LONG: [&str; 11] = [
        "--raw-output",
        "--exit-status",
        "--null-input",
        "--compact-output",
        "--slurp",
        "--join-output",
        "--raw-input",
        "--tab",
        "--sort-keys",
        "--ascii-output",
        "--monochrome-output",
    ];
    let mut programs = 0;
    let mut it = args.iter();
    while let Some(s) = it.next() {
        match *s {
            "--arg" | "--argjson" => {
                let named = it
                    .next()
                    .is_some_and(|n| n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'));
                if !named || it.next().is_none() {
                    return false;
                }
            }
            s if s.starts_with("--") => {
                if !LONG.contains(&s) {
                    return false;
                }
            }
            s if s.starts_with('-') => {
                if s.len() < 2 || !s[1..].chars().all(|c| "rencsjaSCM".contains(c)) {
                    return false;
                }
            }
            s => {
                if !jq_program_reads_only_its_input(s) {
                    return false;
                }
                programs += 1;
            }
        }
    }
    programs == 1
}

fn jq_program_reads_only_its_input(program: &str) -> bool {
    const HOST: [&str; 14] = [
        "modulemeta",
        "localtime",
        "strflocaltime",
        "env",
        "ENV",
        "input_filename",
        "import",
        "include",
        "now",
        "get_search_list",
        "get_prog_origin",
        "get_jq_origin",
        "__loc__",
        "__prog_args",
    ];
    let ident = |c: char| c.is_ascii_alphanumeric() || c == '_';
    !HOST.iter().any(|h| {
        program.match_indices(h).any(|(i, _)| {
            !program[..i].ends_with(ident) && !program[i + h.len()..].starts_with(ident)
        })
    })
}

/// A git read of objects at literally spelled revisions, and nothing
/// about this checkout: not its worktree, index, config-driven diff
/// drivers, local or remote-tracking refs, or a file.
fn git_args(args: &[&str]) -> bool {
    let skip = usize::from(args.first() == Some(&"--no-pager"));
    let Some((sub, rest)) = args[skip..].split_first() else {
        return false;
    };
    match *sub {
        "show" => git_show(rest),
        "log" => git_log(rest),
        "cat-file" => git_cat_file(rest),
        "rev-parse" => git_rev_parse(rest),
        "grep" => git_grep(rest),
        _ => false,
    }
}

/// `git show REV:path`, or a commit with `-s` and a format. A commit's
/// patch is refused: a diff runs the checkout's configured textconv and
/// diff drivers, chosen by its `.gitattributes`.
fn git_show(args: &[&str]) -> bool {
    let (mut no_patch, mut format, mut objects, mut commits) = (false, false, 0, 0);
    for s in args {
        match *s {
            "-s" | "--no-patch" => no_patch = true,
            "--no-color" => {}
            s if is_format(s) => format = true,
            s if s.starts_with('-') => return false,
            s if is_object(s) => objects += 1,
            s if is_rev(s) => commits += 1,
            _ => return false,
        }
    }
    objects + commits > 0 && (commits == 0 || (no_patch && format))
}

/// `git log` at literal revisions, in a format the probe spelled,
/// limited, optionally to paths — never a patch, a stat or a pickaxe
/// (each a diff), never `--all`/`--branches` (the checkout's refs),
/// never `--date=` or `--author=` (the host's TZ, the mailmap), and
/// never `--since`/`--until`/`--after`/`--before` (review 3b63b50f: a
/// relative or local-time date is read against the host's clock and
/// zone, the reason `%ar` is refused).
fn git_log(args: &[&str]) -> bool {
    const FLAGS: [&str; 14] = [
        "--no-merges",
        "--merges",
        "--first-parent",
        "--reverse",
        "--no-color",
        "--all-match",
        "--invert-grep",
        "-i",
        "--regexp-ignore-case",
        "-E",
        "--extended-regexp",
        "-F",
        "--fixed-strings",
        "--topo-order",
    ];
    let mut format = false;
    let mut it = args.iter();
    while let Some(s) = it.next() {
        let ok = match *s {
            "--" => return format && it.all(|p| is_path(p)),
            "-n" => it.next().is_some_and(|n| digits(n)),
            s if is_format(s) => {
                format = true;
                true
            }
            s if FLAGS.contains(&s) || s.starts_with("--grep=") => true,
            s if s.starts_with('-') => {
                s.strip_prefix("--max-count=").is_some_and(digits)
                    || s.strip_prefix("-n").is_some_and(digits)
                    || digits(&s[1..])
            }
            s => is_rev(s),
        };
        if !ok {
            return false;
        }
    }
    format
}

/// `--format=…`, `--pretty=format:…` or `--pretty=tformat:…` printing
/// only what the commit object itself holds. A DEFAULT format is
/// refused because it prints the checkout's notes (refs/notes), and a
/// named one (`--oneline`, `medium`) because `log.decorate` adds its
/// branch names. Placeholders are allowed by name: the full hashes,
/// subject and body, the raw author and committer identity and their
/// epoch or ISO dates. Not `%d`/`%D` (refs), `%N` (notes), `%G…` (a
/// signature checked against the host's keyring), `%(describe)` (tags),
/// `%aN`/`%aE` (the mailmap), `%h`/`%t`/`%p` (an abbreviation as long as
/// THIS repository's object count needs) or `%ad`/`%ar` (`log.date`,
/// the clock).
fn is_format(option: &str) -> bool {
    let Some(format) = option.strip_prefix("--format=").or_else(|| {
        option
            .strip_prefix("--pretty=format:")
            .or_else(|| option.strip_prefix("--pretty=tformat:"))
    }) else {
        return false;
    };
    let mut chars = format.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            continue;
        }
        let ok = match chars.next() {
            Some('%' | 'n' | 'H' | 'T' | 'P' | 's' | 'b' | 'B' | 'f' | 'e') => true,
            Some('a' | 'c') => {
                matches!(chars.next(), Some('n' | 'e' | 'l' | 't' | 'i' | 'I' | 's'))
            }
            Some('x') => (0..2).all(|_| chars.next().is_some_and(|h| h.is_ascii_hexdigit())),
            _ => false,
        };
        if !ok {
            return false;
        }
    }
    true
}

/// `git cat-file -e|-t|-s|-p|<type> <object>` — never `--batch`,
/// `--filters` or `--textconv`.
fn git_cat_file(args: &[&str]) -> bool {
    matches!(args, [mode, object]
        if matches!(*mode, "-e" | "-t" | "-s" | "-p" | "blob" | "tree" | "commit" | "tag")
            && (is_object(object) || is_rev(object)))
}

/// `git rev-parse [--verify] [-q] <revision>…` — never a question about
/// the checkout (`--show-toplevel`, `--git-dir`, `--abbrev-ref`), and
/// never `--short`, whose length is whatever THIS repository's object
/// count makes unambiguous.
fn git_rev_parse(args: &[&str]) -> bool {
    let mut revs = 0;
    for s in args {
        match *s {
            "--verify" | "-q" | "--quiet" => {}
            s if s.starts_with('-') => return false,
            s if is_object(s) || is_rev(s) => revs += 1,
            _ => return false,
        }
    }
    revs > 0
}

/// `git grep [options] <pattern> <revision>… [-- <path>…]` — at least
/// one literal revision, since without one git grep reads the worktree.
fn git_grep(args: &[&str]) -> bool {
    const LONG: [&str; 14] = [
        "--quiet",
        "--count",
        "--files-with-matches",
        "--files-without-match",
        "--name-only",
        "--line-number",
        "--ignore-case",
        "--invert-match",
        "--word-regexp",
        "--fixed-strings",
        "--extended-regexp",
        "--basic-regexp",
        "--perl-regexp",
        "--full-name",
    ];
    let (mut patterns, mut pattern, mut revs) = (false, false, 0);
    let mut i = 0;
    while i < args.len() {
        let s = args[i];
        i += 1;
        if s == "--" {
            return revs > 0 && args[i..].iter().all(|p| is_path(p));
        } else if s.starts_with("--") {
            if s.starts_with("--regexp=") {
                patterns = true;
            } else if !LONG.contains(&s) {
                return false;
            }
        } else if s.len() > 1 && s.starts_with('-') {
            for (at, c) in s.char_indices().skip(1) {
                if "qclLnivwFEGPhHIa".contains(c) {
                    continue;
                }
                if c == 'e' {
                    if s[at + 1..].is_empty() {
                        i += 1;
                        if i > args.len() {
                            return false;
                        }
                    }
                    patterns = true;
                    break;
                }
                return false;
            }
        } else if !patterns && !pattern {
            if s.starts_with('-') {
                return false;
            }
            pattern = true;
        } else if is_object(s) || is_rev(s) {
            revs += 1;
        } else {
            return false;
        }
    }
    revs > 0 && (patterns || pattern)
}

/// `REV:path` — an object named through a literal revision. The path is
/// from the tree's root: not `./` or `../` (relative to wherever the
/// probe stands), not `/`, no `..`.
fn is_object(word: &str) -> bool {
    word.split_once(':').is_some_and(|(rev, path)| {
        !rev.contains("..") && is_rev(rev) && (path.is_empty() || is_path(path))
    })
}

/// A path inside the tree, as a `REV:path` or a pathspec: not an option,
/// not absolute, not `:`-magic, nothing climbing out.
fn is_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with(['-', '/', ':'])
        && !path.contains(['\\', '\n'])
        && path.split('/').all(|seg| seg != ".." && seg != ".")
}

/// A revision spelled literally: `HEAD` or a full 40-digit object name,
/// with `~N`/`^N` suffixes, a leading `^`, or as a `..`/`...` range. A
/// branch — local or `origin/…` — names a different commit in each
/// checkout, and the witness vouches only for HEAD.
fn is_rev(word: &str) -> bool {
    let word = word.strip_prefix('^').unwrap_or(word);
    if let Some((a, b)) = word.split_once("...").or_else(|| word.split_once("..")) {
        return !(a.is_empty() && b.is_empty())
            && (a.is_empty() || is_base_rev(a))
            && (b.is_empty() || is_base_rev(b));
    }
    is_base_rev(word)
}

/// Review 3b63b50f: a 7-39 digit hex name is resolved as a REF first
/// when the checkout has a branch or tag of that name, and can be
/// ambiguous in a larger repository — the reason `%h` is refused. A
/// full 40-digit name is always the object itself.
fn is_base_rev(word: &str) -> bool {
    let cut = word.find(['~', '^']).unwrap_or(word.len());
    let (base, suffix) = word.split_at(cut);
    suffix
        .chars()
        .all(|c| c == '~' || c == '^' || c.is_ascii_digit())
        && (base == "HEAD" || (base.len() == 40 && base.chars().all(|c| c.is_ascii_hexdigit())))
}
#[cfg(test)]
mod tests {
    use super::{reads_only_git_objects, reads_the_clock};

    /// Every spelling bash reads as `%(…)T` is the clock; a literal `%%`
    /// before a parenthesis, and ordinary conversions, are not.
    #[test]
    fn a_clock_conversion_is_found_through_flags_width_and_precision() {
        for f in [
            "%(%s)T",
            "%10(%Y)T",
            "%-(%Y)T",
            "%.4(%Y)T",
            "% (%s)T",
            "%0(%H)T",
            "%#(%Y)T",
            "%+(%Y)T",
            "%5.2(%s)T",
            "%*(%Y)T",
            "%*.*(%Y)T",
            "a %s b %1(%H)T",
            // length modifiers and the `'` flag (review da88bbff)
            "%l(%H)T",
            "%ll(%H)T",
            "%h(%H)T",
            "%hh(%H)T",
            "%L(%H)T",
            "%j(%H)T",
            "%z(%H)T",
            "%t(%H)T",
            "%lh(%H)T",
            "%zj(%H)T",
            "%5l(%H)T",
            "%-5.2l(%H)T",
            "%'(%H)T",
            "%'5(%H)T",
            "%5.2f %l(%H)T",
            "trailing %",
        ] {
            assert!(reads_the_clock(f), "{f}");
        }
        for f in [
            "%s\\n",
            "%%(%s)",
            "100%% (x)",
            "%-10s %5.2f",
            "(%s)",
            "%ld",
            "%'d",
            "%5.2lf",
            "%q",
            "%zu",
            "not yet (%s)",
            "%-10s (%s)",
        ] {
            assert!(!reads_the_clock(f), "{f}");
        }
    }

    /// THE da937eb3 CASE, in the shapes the W41 cars actually recorded
    /// (protocol-retro adc3812b sampled 12; every one read only git
    /// objects at `HEAD`): a blob shown at a ref and filtered on stdin,
    /// guarded by `|| { …; exit 75; }`, under `set -eu`. Each answers the
    /// same in any checkout of the repository that holds the car.
    const RECORDED_SHAPES: &[&str] = &[
        "git show HEAD:crates/core/boss-jobs/src/probe.rs | grep -q 'pub fn x' && echo ok",
        // car eb72a11f, in its shape
        "set -eu\ngit show HEAD:crates/core/boss-jobs/src/move_diagnosis.rs | grep -F 'pub fn validated(' > /dev/null\necho 'present'",
        // car a7f411c2
        "set -euo pipefail\ngit show HEAD:infra/forge/boss-ci/Dockerfile | grep -Fx 'COPY x /usr/bin/kubectl' >/dev/null\necho 'declared'",
        // car 436cb791: matches printed, not discarded
        "set -e\ngit show HEAD:apps/web/tests/mocked/_clock.ts | grep -F 'export async function holdClock('\necho 'present'",
        // car d1a08f2a: a guard per line
        "set -eu\ngit show HEAD:crates/core/boss-jobs/src/move_admission.rs | grep -F 'pub fn observe(' > /dev/null || { echo 'not yet: absent'; exit 75; }\necho ok",
        // car d0eeb107: a `$(` inside SINGLE quotes is inert text
        "set -eu\ngit show HEAD:infra/dev/boss-api | grep -F 'head -c \"$((response_bytes - 4))\" <&\"$response_body\"' >/dev/null\necho ok",
        // car f9ade80a: `--` ends grep's options
        "git show HEAD:infra/forge/probe-bin/boss-sor-read | grep -F -- '--evidence' >/dev/null || exit 75\necho ok",
        // car f117032d: the status of the failed read, reported
        "git show HEAD:infra/estate/registry-manifest.sh | grep -Fq 'registry_blob()' || { echo \"shared blob verifier missing (exit $?)\"; exit 1; }\necho 'PASS'",
        // a literal printf, as the car before 77b35fe5 ended
        "set -eu\ngit show HEAD:x | grep -F 'y' > /dev/null\nprintf '%s\\n' 'exact-move-diagnosis-source:present'",
        "git cat-file -e HEAD:infra/gate.sh && echo ok",
        "git log -1 --format=%s HEAD -- infra/gate.sh | grep -q x && echo ok",
        "git rev-parse --verify HEAD~1 >/dev/null && echo ok",
        "git grep -qF 'fn launch_ref(' HEAD -- crates/x.rs || { echo 'missing on HEAD'; exit 1; }",
        "git show 0123456789abcdef0123456789abcdef01234567:x | grep -q y",
        "#!/usr/bin/env bash\n# reads the tree # only\ngit show HEAD:x | grep -q y && echo ok",
    ];

    #[test]
    fn a_probe_that_reads_only_git_objects_at_a_ref_means_the_same_in_any_checkout() {
        for probe in RECORDED_SHAPES {
            assert!(reads_only_git_objects(probe), "{probe}");
        }
    }

    /// THE COVERAGE THE SHRINK GAVE UP (review 3b63b50f, 2026-10-16).
    /// Each of these was accepted by the shell model before it and reads
    /// only git objects at HEAD — but through a variable, a capture, a
    /// `${…}` default, `case`, `for`, `if` or `[`, the constructs whose
    /// model two reviews found holes in. Refusing them keeps the old
    /// recorded-host rule. Over the 497 newest closed cars, 108 such
    /// probes moved from accepted to refused; none moved the other way.
    const SHAPES_GIVEN_UP: &[&str] = &[
        // car ff1b7a35: a path variable and a loop over literals
        "f=crates/orchestrators/boss-cli/src/dispatch_launch.rs\nfor p in 'async fn fetched_main(' 'fn launch_ref(run_id: &str) -> String {'; do\n  git grep -qF \"$p\" HEAD -- \"$f\" || { echo \"missing on HEAD: $p\"; exit 1; }\ndone\necho ok",
        // car be42f40f: a capture tested by `case`
        "set -eu\ngate=$(git show HEAD:infra/gate.sh) || exit 75\ncase \"$gate\" in *'global_target { next }'*) ;; *) echo 'not yet: absent'; exit 75;; esac\necho ok",
        // car ade746a1: a count guarded by `case` and `[`
        "pin=$(git show HEAD:crates/x.rs 2>/dev/null | grep -c '^fn the_runner')\ncase ${pin:-empty} in empty|*[!0-9]*) echo 'not yet'; exit 75;; esac\n[ \"$pin\" -ge 1 ] || { echo 'not yet'; exit 75; }\necho ok",
        // car 1ba92a79: a capture re-read on stdin
        "set -euo pipefail\nsource=$(git show HEAD:infra/forge/x.sh)\nprintf '%s\\n' \"$source\" | grep -F 'docker run' >/dev/null\necho ok",
        // car 04a83508: jq on a capture
        "row=$(git show HEAD:infra/ops/verbs/x.json) || exit 75\nprintf '%s\\n' \"$row\" | jq -e '.hosts == [\"forge\"]' > /dev/null && echo ok",
        // a path the probe spelled into a variable, quoted and bare
        "p=crates/x.rs\ngit show HEAD:\"$p\" | wc -l",
        "base=crates/core/boss-estate/src\ngit show HEAD:$base/observer.rs 2>&1 | head -n 3",
        "n=$(git grep -c 'fn x' HEAD -- crates/a.rs | wc -l)\n[ \"$n\" -gt 0 ] && echo ok",
        // car 0dccc55e: the door's own instant, guarded before use
        "since=${BOSS_CAR_CONVERGED_AT:-}\ncase ${since:-empty} in empty|*[!0-9]*) echo 'not yet'; exit 75;; esac\ngit show HEAD:x | grep -q y && echo ok",
        // car 048df940: a capture in the first `if` condition
        "if ! body=$(git show HEAD:x 2>&1); then\n  printf '%s\\n' \"$body\"\n  exit 75\nfi\ncase \"$body\" in *'y'*) echo ok;; esac",
        // car 0d1b2f02: `$?` into a variable, a loop, a case pattern
        "body=$(git show HEAD:docs/x.md) || { rc=$?; echo \"unreadable (git exit $rc)\" >&2; exit 75; }\nfor token in 'a' 'b'; do\n  case \"$body\" in *\"$token\"*) ;; *) exit 75;; esac\ndone\necho ok",
        // an `if` over commands the grammar allows: no recorded probe
        // the grammar could otherwise read needed one
        "if git show HEAD:x | grep -q y; then echo ok; else echo 'not yet'; exit 75; fi",
    ];

    #[test]
    fn a_probe_that_reads_git_objects_through_the_shell_keeps_the_host_rule() {
        for probe in SHAPES_GIVEN_UP {
            assert!(!reads_only_git_objects(probe), "{probe:?}");
        }
    }

    /// FAIL CLOSED: anything that could read the HOST, or a git read
    /// whose answer depends on the checkout rather than on the objects
    /// at a named ref, keeps the recorded-host rule. The cost of a miss
    /// here is the old CANNOT RE-RUN HERE; the cost of a false accept
    /// would be a false NO LONGER HOLDS.
    #[test]
    fn a_git_probe_that_could_read_the_host_or_the_worktree_keeps_the_host_rule() {
        for probe in [
            // the WORKTREE, not an object at a ref
            "git grep -q x && echo ok",
            "git grep -q 'pub fn x' -- crates && echo ok",
            "git grep -e foo -e HEAD && echo ok",
            "git grep -q x --no-index HEAD",
            "git grep -f patterns HEAD",
            "git show :crates/x.rs | grep -q y",
            "git diff HEAD | grep -q y",
            "git status --short",
            // a ref the probe does not spell
            "git show \"$ref:crates/x.rs\"",
            "git show $REF:crates/x.rs",
            "ref=${BOSS_SOURCE_REF:-HEAD}\ngit show \"$ref:x\" | grep -q y",
            "ref=$(git rev-parse HEAD)\ngit show \"$ref:x\"",
            "git cat-file -e origin/main:infra/gate.sh && echo ok",
            "git log -1 --format=%s origin/main -- infra/gate.sh",
            // a short hex name, which a ref of that name shadows
            "git show 0123456:x",
            "git show 0123456789abcdef0123456789abcdef0123456:x",
            "git show 0123456789abcdef0123456789abcdef012345678:x",
            // checkout facts that differ by host
            "git rev-parse --show-toplevel",
            "git rev-parse --abbrev-ref HEAD",
            "git log --all --oneline | grep -q x",
            "git -C /var/lib/boss-train/repo show HEAD:x",
            "git -c core.pager=x show HEAD:x",
            // a patch runs the checkout's diff drivers
            "git show HEAD | grep -q x",
            "git log -p -1 HEAD | grep -q x",
            // the clock and the zone
            "git log -1 --format=%H --until=yesterday HEAD",
            "git log -1 --format=%H --after=@0 HEAD",
            "git log -1 --format=%H --before=2026-10-01 HEAD",
            "git log -1 --format=%H --max-age=0 HEAD",
            "git show HEAD:x; printf 'at %(%s)T\\n' -1",
            "git show HEAD:x; printf '%s %(%F)T\\n' a -1",
            // a flag, width or precision before `(` still reads the clock
            // (review 8d6976fa finding 1)
            "git show HEAD:x; printf '%10(%Y)T' -1",
            "git show HEAD:x; printf '%l(%H)T' -1",
            "git show HEAD:x; printf '%z(%H)T' | grep -q 1",
            "git show HEAD:x; printf '%-(%Y)T' -1",
            "git show HEAD:x; printf '%.4(%Y)T' -1",
            "git show HEAD:x; printf '% (%s)T' -1",
            "git show HEAD:x; printf '%0(%H)T' -1",
            "git show HEAD:x; printf '%#(%Y)T' -1",
            "git show HEAD:x; printf '%+(%Y)T' -1",
            "git show HEAD:x; printf '%5.2(%s)T' -1",
            "git show HEAD:x; printf '%*(%Y)T' 4 -1",
            "git show HEAD:x; printf \"%10(%Y)T\" -1",
            "git show HEAD:a | grep -q x || { printf '%1(%H)T' | grep -q 1 && exit 75; }",
            // the HOST
            "git show HEAD:x | grep -q y; cat /etc/boss/sor.env",
            "grep -q y crates/x.rs",
            "git show HEAD:x > /tmp/y; grep -q z /tmp/y",
            "git show HEAD:x | grep -f /etc/patterns",
            "git show HEAD:x | grep -rq y",
            "jq -r .a < /etc/x.json; git show HEAD:x",
            "git show HEAD:x | jq --slurpfile a /etc/x .",
            "git show HEAD:x | jq -r env.HOME",
            "[ -f /etc/x ] && git show HEAD:x",
            "git show HEAD:x | grep -q \"`cat /etc/x`\"",
            "git show HEAD:x | grep -q \"$(cat /etc/x)\"",
            "git show HEAD:x | grep -F -- y /etc/x",
            "record=$(mktemp)\ngit show HEAD:x > \"$record\"\ngrep -F y \"$record\"",
            "git show HEAD:x | sed -n '1p'",
            "ssh boss-gcp git show HEAD:x",
            "x=\"$(cat /etc/x)\"; git show HEAD:x",
            "#!/usr/bin/env bash\nsystemctl is-active boss && git show HEAD:x",
            "for f in *.rs; do git show HEAD:x; done",
            "'git' show HEAD:x",
            "git show HEAD:x; set",
            "git show HEAD:x; set -o",
            "git show HEAD:x; set -oo pipefail pipefail",
            "git show HEAD:x; set -o posix",
            // `$?` is a number only where echo prints it
            "git show HEAD:x; echo $?",
            "git show HEAD:x | grep -q \"$?\"",
            "git show HEAD:x; exit \"$?\"",
            "git show HEAD:x; printf \"$?\"",
            "git show HEAD:x; git show \"HEAD:$?\"",
            "git show HEAD:x; echo \"$_\"",
            "git show HEAD:x; echo \"\\$HOME\"",
            // any variable at all, the probe's own included
            "git show HEAD:x | grep -q \"$p\"",
            "p=1\ngit show HEAD:x | grep -q \"$p\"",
            "git show HEAD:x; echo $BOSS_CAR_CONVERGED_AT",
            "git show HEAD:x; echo \"$BOSS_CAR_MERGE_REF\"",
            "BOSS_CAR_CONVERGED_AT=1\ngit show HEAD:x",
            // a subshell, a `case`, a test
            "( git show HEAD:x )",
            "case x in x) git show HEAD:x;; esac",
            "git show HEAD:x; [ x = y ]",
            "git show HEAD:x; test -n x",
            // a computed value
            "n=$(git show HEAD:x | grep -c y)\necho n=$n",
            // a loop that can run forever
            "while true; do git show HEAD:x; done",
            // a tilde prefix and a brace expansion
            "git show HEAD:x; echo ~",
            "git show HEAD:x; echo a=~",
            "git show HEAD:x; echo a:~",
            "git show HEAD:x; echo /~",
            "git show HEAD:x; echo {a,b}",
            "git show HEAD:x; echo }{",
            // the system of record has its own classifier
            "boss-sor-read /api/x | jq .; git show HEAD:x",
            // no git object read at all
            "echo ok",
            "",
        ] {
            assert!(!reads_only_git_objects(probe), "{probe:?}");
        }
    }

    /// REVIEW 2c318e8f, PORTED WHOLE. The reviewer fed 173 hostile
    /// strings to the first cut through a throwaway test, and five
    /// classes passed that read the host, another repository or a file:
    /// an assignment before `git`, a globbed or numeric program word, an
    /// attached `-O<file>`, the host's variables, and output redirected
    /// to a file. Every string is kept here, so a later edit that reopens
    /// one of the classes fails by name. The ones still accepted are
    /// reads of objects at `HEAD`, and nothing else.
    const REVIEW_ACCEPTED: &[&str] = &[
        "git show HEAD:x; true",
        "git show HEAD:x 1>&2",
        "git show HEAD:x | head -c 3",
        "git show HEAD:x | grep -A1 y",
        "git grep x HEAD -- x",
        "git cat-file -p HEAD:x",
        "git cat-file blob HEAD:x",
        "git cat-file -e HEAD",
        "git --no-pager show HEAD:x",
        "git show HEAD:x | wc -l",
        "git show HEAD:x | grep -q y && echo ok",
        "git show HEAD:x | jq -r .name",
        "git log -1 --format=%H HEAD",
        "git show -s --format=%s HEAD",
        // the review's default-format `git log` reads, given a format
        "git log --format=%H HEAD -- x",
        "git log --format=%H -- x",
        "git log --format=%H ^HEAD",
        "git log --pretty=format:%H HEAD..",
        "git rev-parse HEAD",
    ];

    const REVIEW_REFUSED: &[&str] = &[
        "git -C /tmp show HEAD:x",
        "git --git-dir=/x show HEAD:x",
        "git -c core.pager=sh show HEAD:x",
        "git -c alias.show=!sh show HEAD",
        "git show --output=/scratch/m HEAD",
        "git show --ext-diff HEAD",
        "git show --textconv HEAD:x",
        "git grep -O sh x HEAD",
        "git grep --open-files-in-pager=sh x HEAD",
        "git log --exec=sh HEAD",
        "git show HEAD:x > /scratch/m",
        "git show HEAD:x >> /scratch/m",
        "git show HEAD:x && id",
        "git show $REV:x",
        "git show \"$(id)\":x",
        "git show $'\\x48EAD':x",
        "git show HEAD:x | sh",
        "git show HEAD:x | xargs true",
        "git show main:x",
        "git show HEAD:x <<EOF",
        "git show HEAD:* ",
        "git show `id`:x",
        "git cat-file --batch",
        "git cat-file --filters HEAD:x",
        "git rev-parse --git-path x",
        "git log -p --output=/scratch/m HEAD",
        // abbreviated long options
        "git show --outp=/scratch/m HEAD",
        "git show --output-dir=/scratch/m HEAD",
        "git log --ext-d -p HEAD",
        "git show --textc HEAD:x",
        "git show --textconv=yes HEAD:x",
        "git log -p --textconv HEAD",
        "git log --branch HEAD",
        "git log --al HEAD",
        "git show --format=%x HEAD --all",
        "git log --since=x --stdi HEAD",
        "git log --no-walk --not --all HEAD",
        "git show --no-ind HEAD",
        "git log -L1,2:x HEAD",
        "git log --follow HEAD -- x",
        "git show -O/scratch/m HEAD",
        "git log --source HEAD",
        "git grep --no-inde x HEAD",
        "git grep --untr x HEAD",
        "git grep --open-files x HEAD",
        "git grep --fil=x HEAD",
        "git grep --cach x HEAD",
        "git grep --recurse x HEAD",
        "git grep -lO x HEAD",
        "git grep x HEAD --file=y",
        // environment assignments ahead of git
        "GIT_DIR=/elsewhere git show HEAD:x",
        "GIT_TRACE=/scratch/m git show HEAD:x",
        "GIT_EXTERNAL_DIFF=true git show HEAD",
        "GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=a.b GIT_CONFIG_VALUE_0=c git show HEAD:x",
        "GIT_WORK_TREE=/x git grep x HEAD",
        "GIT_INDEX_FILE=/x git show HEAD:x",
        // program positions the scan skipped
        "git show HEAD:x; ${X:-true} arg",
        "git show HEAD:x; /bin/tru? arg",
        "git show HEAD:x; /bin/tru* arg",
        "git show HEAD:x; ./a:b arg",
        "git show HEAD:x; 1 arg",
        // quote tracker vs comments
        "git show HEAD:x | grep -q y # don't\nid\n# '",
        "git show HEAD:x # it's\nid\n#'",
        "# what's this\nid\n# '\ngit show HEAD:x",
        "git show HEAD:x | grep -q \"y\" # \"\nid\n# \"",
        // backslash and keyword tricks
        "git show HEAD:x; \\id",
        "git show HEAD:x; i\\d",
        "git show HEAD:x; time id",
        "git show HEAD:x; ! id",
        "git show HEAD:x; -x id",
        "git show HEAD:x; exec id",
        "git show HEAD:x; eval id",
        "git show HEAD:x; command id",
        "git show HEAD:x; . /scratch/m",
        "git show HEAD:x; source /scratch/m",
        "git show HEAD:x; if id; then :; fi",
        "git show HEAD:x; while id; do :; done",
        "git show HEAD:x; case x in x) id;; esac",
        "git show HEAD:x; for f in 1; do id; done",
        "git show HEAD:x; f() { id; }; f",
        "git show HEAD:x; echo $(id)",
        "git show HEAD:x; echo \"$(id)\"",
        "git show HEAD:x; echo 'a' $(id)",
        "git show HEAD:x; echo hi > /scratch/m",
        "git show HEAD:x; echo hi >| /scratch/m",
        "git show HEAD:x; echo hi &> /scratch/m",
        "git show HEAD:x >& /scratch/m",
        "git show HEAD:x | tee /scratch/m",
        "git show HEAD:x | head x",
        "git show HEAD:x | wc -l x",
        "git show HEAD:x | tail -f",
        "git show HEAD:x | grep -r y",
        "git show HEAD:x | grep -R y",
        "git show HEAD:x | grep -d recurse y .",
        "git show HEAD:x | grep -e y .",
        "git show HEAD:x | grep --binary-files=text y",
        "git show HEAD:x | grep -- y z",
        "git show HEAD:x | jq -f prog",
        "git show HEAD:x | jq --rawfile a /etc/x .",
        "git show HEAD:x | jq -L /x .",
        "git show HEAD:x | jq --from-file p",
        "git show HEAD:x | jq . /etc/x",
        "git show HEAD:x | jq --arg a b . /etc/x",
        "git show HEAD:x | jq 'input_filename'",
        "[ -f /etc/x ]",
        "git show HEAD:x; [ /etc/x -nt /etc/y ]",
        "git show HEAD:x; [[ -v X ]]",
        "git show HEAD:x; test -n x -o -f /etc/x",
        "git show HEAD:x; printf -v X y",
        "git show HEAD:x; set -o x",
        "git show HEAD:x; echo $((1+1))",
        "git show HEAD:x; echo ${X?}",
        "git show HEAD:x; echo ~/x",
        "git show HEAD:x; echo *",
        "git show HEAD:x & id",
        "git show HEAD:x || id",
        "git show HEAD:x |& id",
        "git show origin/../x:y",
        "git show origin/x/../../y:z",
        "git show HEAD@{1}:x",
        "git show HEAD:../x",
        "git show origin/main:x origin/main~1",
        "git show 1234567",
        "git show HEAD^{tree}",
        "git show HEAD^!",
        "git show -- HEAD:x",
        "git show HEAD:x -- y",
        "git log ..",
        "git log ..origin/x",
        "git log FETCH_HEAD",
        "git log ORIG_HEAD",
        "git log origin/HEAD",
        "git rev-parse --verify origin/main",
        "git rev-parse --git-dir",
        "git rev-parse --local-env-vars HEAD",
        "git cat-file --textconv HEAD:x",
        "git --no-pager --git-dir=/x show HEAD:x",
        "git notes show HEAD",
        "git show-ref",
        "git --exec-path=/x show HEAD:x",
        "git show HEAD:x 2>/scratch/m",
        "git show HEAD:x 2> /scratch/m",
        "git show HEAD:x>/scratch/m",
        "git show HEAD:x >",
        "git show HEAD:x > > /scratch/m",
        // the host's variables (review_test2)
        "[ \"$HOSTNAME\" = h ] && git show HEAD:x",
        "case $HOSTNAME in h) exit 1;; esac; git show HEAD:x",
        "git show HEAD:x | grep -q \"$HOME\"",
        "git show HEAD:x; echo $PWD",
        "git show HEAD:x; [ -n \"$SSH_AUTH_SOCK\" ]",
        "X=1 git show HEAD:x",
        "env git show HEAD:x",
        "git show -Oorder HEAD",
        "git show -O order HEAD",
        "git log -p -O/etc/x HEAD",
        // accepted by the first cut, refused now: a default format prints
        // the checkout's notes, `--short` is as long as its object count
        // needs
        "git log HEAD -- x",
        "git log",
        "git log -- x",
        "git log ^HEAD",
        "git log HEAD..",
        "git rev-parse --short HEAD",
        "git log --oneline -1 HEAD",
        "git show -s HEAD",
        "git log -1 --format=%d HEAD",
        "git log -1 --format=%h HEAD",
        "git log -1 --format=%N HEAD",
        "git log -1 --format=%G? HEAD",
        "git log -1 --format=%(describe) HEAD",
        "git log -1 --format=%aN HEAD",
        "git log -1 --author=x --format=%H HEAD",
        "git log -1 --pretty=medium HEAD",
        // arithmetic evaluates a value from the environment
        "((x=y)); git show HEAD:x",
        "git show HEAD:x | jq -r 'now'",
        // the review's non-blocking classes
        "git cat-file -e origin/main:infra/gate.sh",
        "git grep --recurse-submodules x HEAD",
        "x=HEAD; GIT_DIR=/x; git show \"$x:y\"",
        "git show HEAD:x; for f in ~; do :; done",
        // accepted by the shell model, refused by the grammar that
        // replaced it (review 3b63b50f): a short hex name, a loop, a test
        "git show deadbeef:x",
        "git show HEAD:x; for f in Q; do :; done",
        "git show HEAD:x; [ x = y ]",
    ];

    #[test]
    fn every_string_review_2c318e8f_fed_the_classifier_keeps_its_verdict() {
        let wrongly_refused: Vec<&str> = REVIEW_ACCEPTED
            .iter()
            .copied()
            .filter(|p| !reads_only_git_objects(p))
            .collect();
        let wrongly_accepted: Vec<&str> = REVIEW_REFUSED
            .iter()
            .copied()
            .filter(|p| reads_only_git_objects(p))
            .collect();
        assert!(
            wrongly_refused.is_empty() && wrongly_accepted.is_empty(),
            "refused, should accept: {wrongly_refused:#?}\naccepted, should refuse: {wrongly_accepted:#?}"
        );
    }

    /// Each of the five blocking classes, named, with the shape that
    /// is still accepted beside it — so the rule is visibly the cause,
    /// not a lexer that happens to choke on the hostile half.
    #[test]
    fn each_blocking_class_of_review_2c318e8f_is_refused_beside_its_safe_twin() {
        for (refused, accepted) in [
            // 1. an environment for git
            ("GIT_DIR=/elsewhere git show HEAD:x", "git show HEAD:x"),
            // 2. a program word the shell expands
            ("git show HEAD:x; /bin/tru? arg", "git show HEAD:x; true"),
            // 3. an attached option nobody listed
            ("git show -Oorder HEAD", "git show -s --format=%H HEAD"),
            // 4. a variable the host set
            (
                "git show HEAD:x | grep -q \"$HOME\"",
                "git show HEAD:x | grep -q 'y'",
            ),
            // 5. output to a file
            (
                "git show HEAD:x > /scratch/m",
                "git show HEAD:x > /dev/null",
            ),
        ] {
            assert!(!reads_only_git_objects(refused), "{refused}");
            assert!(reads_only_git_objects(accepted), "{accepted}");
        }
    }

    /// REVIEW 3b63b50f, PORTED WHOLE: the 150 strings the second reviewer
    /// fed the shell model, verbatim. Its nine accepted holes are at the
    /// top of the refusals. Every string that used a variable, `$(…)`,
    /// `${…}`, `case`, `for`, `if` or `[` is refused now, safe or not:
    /// that is the grammar shrinking, not a verdict on each string.
    const REVIEW_3B63B50F_ACCEPTED: &[&str] = &[
        "git show HEAD:a; echo '$(id)'",
        "{ git show HEAD:a; } 2>/dev/null",
        "git show HEAD:a | jq --arg n v '.x == $n'",
        "git show HEAD:a | jq -n 'input'",
        "git show HEAD:a | jq '[inputs]'",
        "git show HEAD:a | jq 'getpath([\"x\"])'",
        "git show -s --format=%H HEAD",
        "git rev-parse HEAD",
        "git rev-parse --verify -q HEAD~1",
        "git cat-file -p HEAD:a",
        "git grep -qF x HEAD -- a",
        "git --no-pager show HEAD:a",
        "git show HEAD:a > /dev/null 2>&1",
        "git show HEAD:a &> /dev/null",
        "git show HEAD:a | head -n 3",
        "git show HEAD:a | wc -l",
        "git show HEAD:a | grep -e x -e y",
        "git show HEAD:a; set -o pipefail",
        "git show HEAD:a # ; id",
        "git show 'HEAD:a' \"HEAD:b\"",
    ];

    const REVIEW_3B63B50F_REFUSED: &[&str] = &[
        "git rev-parse HEAD >/dev/null; set | grep -q '^HOSTNAME=boss-dev' && echo pod",
        "git rev-parse HEAD >/dev/null; set | grep -c HOME",
        "git rev-parse HEAD >/dev/null\nx=\ny=${x:-~}\necho \"$y\"",
        "x=\ny=${x:-~}\ngit show HEAD:a | grep -qF \"$y\" && echo ok",
        "x=\ncase ${x:-~} in /work*) echo pod;; *) echo other;; esac\ngit show HEAD:a",
        "_=HEAD:a\necho --output=out.txt\ngit show HEAD:a $_",
        "_=HEAD:a\necho \"$(git show HEAD:a)\" >/dev/null\ngit show HEAD:a $_",
        "git rev-parse HEAD >/dev/null; printf '%(%Y-%m-%d %H)T\\n' -1",
        "git log -1 --format=%H --since=2.weeks.ago HEAD",
        "x=\"$(echo \"$(git show HEAD:a)\")\"; echo \"$x\"",
        "x=\"$(git show HEAD:a | jq -r \"$(echo .x)\")\"",
        "echo \"$(git show HEAD:a)\" $(git show HEAD:b)",
        "x=${y:-$(id)}; git show HEAD:a",
        "y=a; x=${y:-$(id)}; git show HEAD:a",
        "y=a; x=\"${y:-`id`}\"; git show HEAD:a",
        "git show HEAD:a; echo \"\\$(id)\"",
        "case x in x) id;; esac; git show HEAD:a",
        "case x in x) git show HEAD:a;; esac",
        "for f in $(git ls-files); do git show HEAD:$f; done",
        "for f in a b; do git show HEAD:$f; done",
        "for f in a b; do git show \"HEAD:$f\"; done",
        "for f in 'a b' c; do git show HEAD:$f; done",
        "if id; then git show HEAD:a; fi",
        "if git show HEAD:a | grep -q x; then echo ok; else exit 1; fi",
        "f(){ git show HEAD:a; }; f",
        "function f { git show HEAD:a; }; f",
        "{ git show HEAD:a; } > out.txt",
        "( cd /; git show HEAD:a )",
        "while true; do git show HEAD:a; done",
        "git show HEAD:a; [ $? -eq 0 ] && echo ok",
        "git show HEAD:a; git show $?",
        "x=$HOME; git show $x",
        "x=HEAD:a; git show $x",
        "x=$(git rev-parse HEAD); git show $x:a",
        "x=$(git rev-parse HEAD); git show \"$x:a\"",
        "false && x=HEAD:a; git show $x",
        "( x=HEAD:a ); git show $x",
        "x=HEAD:a | true; git show $x",
        "path=/etc/passwd; git show HEAD:$path",
        "path=..; git show HEAD:$path/x",
        "ifs=x; git show HEAD:a",
        "IFS=x; git show HEAD:a",
        "git_dir=/x; git show HEAD:a",
        "git show HEAD:a | jq '$__loc__'",
        "git show HEAD:a | jq '$ENV.HOME'",
        "git show HEAD:a | jq 'env.HOME'",
        "git show HEAD:a | jq -L /tmp 'import \"m\" as m; .'",
        "git show HEAD:a | jq --rawfile f /etc/passwd '$f'",
        "git show HEAD:a | jq --slurpfile f /etc/passwd '$f'",
        "git show HEAD:a | jq --args '$ARGS' x",
        "git show HEAD:a | jq . /etc/passwd",
        "git show HEAD:a | jq -f prog.jq",
        "git show HEAD:a | jq 'input_filename'",
        "git show HEAD:a | jq 'now'",
        "e=env; git show HEAD:a | jq \"$e\"",
        "e=en; git show HEAD:a | jq \"${e}v\"",
        "git show -s --format='%(describe)' HEAD",
        "git show -s --format=%Cred%H HEAD",
        "git show -s --format=%H --notes HEAD",
        "git show -s --format=%H --show-signature HEAD",
        "git show -s --format=%N HEAD",
        "git show -s --format=%GG HEAD",
        "git show --textconv HEAD:a",
        "git show --output=out.txt HEAD:a",
        "git show HEAD",
        "git log --follow --format=%H HEAD -- a",
        "git log -L 1,2:a --format=%H HEAD",
        "git log --format=%H --all",
        "git log --format=%H -g",
        "git log --format=%H --output=out.txt HEAD",
        "git log --format=%H HEAD@{1}",
        "git log --format=%H origin/main",
        "git rev-parse --show-toplevel",
        "git rev-parse --local-env-vars",
        "git rev-parse --git-path hooks",
        "git cat-file --batch",
        "git cat-file --textconv HEAD:a",
        "git grep -O id x HEAD",
        "git grep --open-files-in-pager=id x HEAD",
        "git grep -p x HEAD",
        "git grep -W x HEAD",
        "git grep x",
        "git grep --cached x",
        "git grep --untracked x",
        "git grep --no-index x",
        "git grep -f pats HEAD",
        "git grep x HEAD -- ':(top)a'",
        "git grep x HEAD -- ../a",
        "git -c core.pager=id show HEAD:a",
        "git -C / show HEAD:a",
        "git --git-dir=/x show HEAD:a",
        "git show HEAD:./a",
        "git show HEAD:../a",
        "git show :a",
        "git show HEAD^{tree}",
        "git show 'HEAD@{yesterday}:a'",
        "git notes show HEAD",
        "git config --get user.name",
        "git status",
        "git diff HEAD",
        "git ls-files",
        "git show HEAD:a >> out.txt",
        "git show HEAD:a 3>/dev/null",
        "git show HEAD:a >&3",
        "git show HEAD:a > /dev/tcp/1.2.3.4/80",
        "git show HEAD:a | tee out.txt",
        "git show HEAD:a | tail -f",
        "git show HEAD:a | grep -r x .",
        "git show HEAD:a | grep x /etc/passwd",
        "git show HEAD:a; printf -v x '%s' y",
        "git show HEAD:a; set +o",
        "git show HEAD:a; set -a",
        "git show HEAD:a; [ -f /etc/passwd ]",
        "git show HEAD:a; test -v HOME",
        "git show HEAD:a; [[ $HOME ]]",
        "git show HEAD:a; exec id",
        "git show HEAD:a; eval id",
        "git show HEAD:a; . ./x",
        "git show HEAD:a; time id",
        "git show HEAD:a & wait",
        "git show HEAD:a; echo ${#HOME}",
        "git show HEAD:a; echo ${!x}",
        "git show HEAD:a; echo $'\\x41'",
        "git show HEAD:a; echo $((1+1))",
        "git show HEAD:a; echo ~",
        "git show HEAD:a; echo a{b,c}",
        "git show HEAD:a; echo /bin/i?",
        "git show HEAD:a; echo \"$BOSS_CAR_MERGE_REF\"",
        "git show $BOSS_CAR_MERGE_REF:a",
        "git show HEAD:a\\\n; id",
    ];

    #[test]
    fn every_string_review_3b63b50f_fed_the_classifier_keeps_its_verdict() {
        let wrongly_refused: Vec<&str> = REVIEW_3B63B50F_ACCEPTED
            .iter()
            .copied()
            .filter(|p| !reads_only_git_objects(p))
            .collect();
        let wrongly_accepted: Vec<&str> = REVIEW_3B63B50F_REFUSED
            .iter()
            .copied()
            .filter(|p| reads_only_git_objects(p))
            .collect();
        assert!(
            wrongly_refused.is_empty() && wrongly_accepted.is_empty(),
            "refused, should accept: {wrongly_refused:#?}\naccepted, should refuse: {wrongly_accepted:#?}"
        );
        assert_eq!(
            REVIEW_3B63B50F_ACCEPTED.len() + REVIEW_3B63B50F_REFUSED.len(),
            150
        );
    }

    /// The holes review 3b63b50f found in the shell model, one per line,
    /// each named — the four blocking ones and the two it recorded as
    /// follow-ups (a relative date, a short hex name).
    #[test]
    fn each_hole_review_3b63b50f_found_in_the_shell_model_is_refused() {
        for hole in [
            // 1. `$_` is the shell's, and carried a write option to git
            "_=HEAD:a\necho --output=out.txt\ngit show HEAD:a $_",
            "_=HEAD:a\necho \"$(git show HEAD:a)\" >/dev/null\ngit show HEAD:a $_",
            // 2. `~` in a default expands to $HOME
            "git rev-parse HEAD >/dev/null\nx=\ny=${x:-~}\necho \"$y\"",
            "x=\ny=${x:-~}\ngit show HEAD:a | grep -qF \"$y\" && echo ok",
            "x=\ncase ${x:-~} in /work*) echo pod;; *) echo other;; esac\ngit show HEAD:a",
            // 3. a bare `set` prints the environment
            "git rev-parse HEAD >/dev/null; set | grep -q '^HOSTNAME=boss-dev' && echo pod",
            "git rev-parse HEAD >/dev/null; set | grep -c HOME",
            // 4. `%(…)T` reads the clock
            "git rev-parse HEAD >/dev/null; printf '%(%Y-%m-%d %H)T\\n' -1",
            // 5. a relative date reads the clock
            "git log -1 --format=%H --since=2.weeks.ago HEAD",
            // 6. a short hex name resolves a ref of that name first
            "git show deadbeef:x",
        ] {
            assert!(!reads_only_git_objects(hole), "{hole:?}");
        }
    }

    /// A REFUSAL SAYS WHERE THE TEXT LEFT THE GRAMMAR (backlog 70cb74d8,
    /// from reviews 8d6976fa, da88bbff and 1042cf88 of car afd61aa2).
    /// `Refused` carried nothing, so `boss prove --recheck` could only
    /// list every cause it might have been. Each class answers with the
    /// evidence an operator needs to rewrite the probe: the word and the
    /// character an expansion or a redirection starts at, the command
    /// whose program or arguments the grammar does not allow, the end of
    /// a text that stops where a command must follow, and a text that
    /// fits but runs no `git`.
    #[test]
    fn a_refusal_names_where_the_text_left_the_grammar() {
        use super::{Refused, git_objects_only};
        let shell = |at: usize, near: &str| Refused::Shell {
            at,
            near: near.into(),
        };
        let command = |c: &str| Refused::Command { command: c.into() };
        for (probe, why) in [
            // an expansion, at the word that carries it
            (
                "git show HEAD:x | grep -q \"$HOME\"",
                shell(26, "\"$HOME\""),
            ),
            // `near` stops at the end of the line
            (
                "git show HEAD:a; echo ${#HOME}\necho ok",
                shell(22, "${#HOME}"),
            ),
            // a redirection to a file, at its `>`
            ("git show HEAD:a > /scratch/m", shell(16, "> /scratch/m")),
            // a subshell
            (
                "( cd /; git show HEAD:a )",
                shell(0, "( cd /; git show HEAD:a )"),
            ),
            // a connective with no command after it
            ("git show HEAD:a &&", shell(18, "")),
            // a program the grammar does not run
            (
                "curl -s http://x | grep -q y; git show HEAD:a",
                command("curl -s http://x"),
            ),
            // an option git is not allowed
            (
                "git show --output=out.txt HEAD:a",
                command("git show --output=out.txt HEAD:a"),
            ),
            // a reserved word is a program the grammar does not run
            ("if id; then git show HEAD:a; fi", command("if id")),
            // the whole text fits, and reads no git object at all
            ("echo ok", Refused::NoGit),
        ] {
            assert_eq!(git_objects_only(probe), Err(why), "{probe:?}");
        }
        // A long line is cut, and says so.
        let long = format!("git show HEAD:a; echo $HOME{}", "x".repeat(80));
        match git_objects_only(&long) {
            Err(Refused::Shell { at: 22, near }) => {
                assert!(near.starts_with("$HOMEx") && near.ends_with('…'), "{near}");
                assert!(near.chars().count() <= 41, "{near}");
            }
            other => panic!("{other:?}"),
        }
    }

    /// The sentence `boss prove` prints names the evidence, counted from 1
    /// as an editor counts.
    #[test]
    fn a_refusal_reads_as_a_sentence_naming_its_evidence() {
        use super::Refused;
        let s = Refused::Shell {
            at: 26,
            near: "\"$HOME\"".into(),
        }
        .to_string();
        assert!(
            s.contains("`\"$HOME\"`") && s.contains("character 27"),
            "{s}"
        );
        let end = Refused::Shell {
            at: 18,
            near: String::new(),
        }
        .to_string();
        assert!(
            end.contains("ends") && end.contains("character 19"),
            "{end}"
        );
        let c = Refused::Command {
            command: "curl -s http://x".into(),
        }
        .to_string();
        assert!(c.contains("`curl -s http://x`"), "{c}");
        assert!(Refused::NoGit.to_string().contains("no `git`"));
    }

    /// EVERY REFUSAL QUOTES THE PROBE'S OWN TEXT, across every string the
    /// reviews fed the classifier: a `Shell` excerpt is the text at its
    /// character, and a `Command` is spelled as the probe spells it — so
    /// what an operator reads can be found in the probe they hold.
    #[test]
    fn every_refusal_quotes_the_probes_own_text() {
        use super::{Refused, git_objects_only};
        for probe in SHAPES_GIVEN_UP
            .iter()
            .chain(REVIEW_REFUSED)
            .chain(REVIEW_3B63B50F_REFUSED)
        {
            let chars: Vec<char> = probe.chars().collect();
            match git_objects_only(probe) {
                Ok(()) => panic!("accepted: {probe:?}"),
                Err(Refused::Shell { at, near }) => {
                    let quoted: String = chars[at.min(chars.len())..].iter().collect();
                    assert!(
                        quoted.starts_with(near.trim_end_matches('…')),
                        "{probe:?}: {near:?} is not the text at character {at}"
                    );
                }
                Err(Refused::Command { command }) => assert!(
                    probe.contains(command.trim_end_matches('…')),
                    "{probe:?}: {command:?}"
                ),
                Err(Refused::NoGit) => assert!(!probe.contains("git "), "{probe:?}"),
            }
        }
    }
}
