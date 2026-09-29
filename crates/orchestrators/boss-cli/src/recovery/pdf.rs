//! `boss recovery sheet --pdf` — the sheet as the paper David prints
//! (design 125d405d §6, car 2 of backlog fd6d6c08).
//!
//! PRINTED BY THE CHROMIUM THE BOSS-CI IMAGE ALREADY BAKES. The image
//! installs Playwright's browsers at /opt/ms-playwright
//! (infra/forge/boss-ci/Dockerfile, PLAYWRIGHT_BROWSERS_PATH) for the
//! mocked web suite, and the dev pod carries the same tree. Playwright's
//! `page.pdf` is Chromium's own print-to-PDF driven over the DevTools
//! protocol; this runs the same print from Chromium's command line, so
//! the renderer needs the browser and nothing else — no node_modules,
//! no script, no new toolchain. The headless shell is preferred because
//! it is the binary Playwright's headless `page.pdf` launches.
//!
//! READ BACK, NOT ASSUMED. A PDF is bytes a reader cannot check by
//! eye before it is on paper, so the verb reads the text back out of the
//! PDF it made, each piece with its place on its page ([`runs`]), and
//! refuses to hand over a page that does not carry every value `--facts`
//! prints, or whose footer band holds anything but the footer
//! (`Rendered::judge_print`). The print and its proof are one act.
//!
//! THE READER IS FOR THIS PRINTER'S OUTPUT, NOT FOR PDFS. Chromium's
//! PDF backend (Skia) writes plain numbered objects, Flate streams, and
//! Type0 fonts with a ToUnicode map — the shape [`runs`] reads. Anything
//! else (object streams, an indirect stream length, a font without a
//! map) is refused by name rather than read as empty: a reader that
//! answered "no text" would make every fact look missing, or, worse, a
//! reader that skipped a font would drop exactly the words it could not
//! read.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::LazyLock;
use std::time::{Duration, Instant};

use regex::bytes::Regex as BytesRegex;

/// A print that has not finished in this long is a stuck browser: the
/// whole sheet prints in about a second on the dev pod (2026-09-29).
const PRINT_DEADLINE: Duration = Duration::from_secs(60);

/// The most one stream may inflate to. The sheet's largest page stream
/// is a few kilobytes and its largest font file under 30 KB inflated
/// (2026-09-29); a stream past this is not this printer's.
const INFLATE_CAP: u64 = 64 * 1024 * 1024;

/// Where Playwright puts its browsers, in the order to look: the image's
/// declared path, the path the boss-ci image bakes, then a user's cache.
fn browser_roots() -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = std::env::var_os("PLAYWRIGHT_BROWSERS_PATH")
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        .into_iter()
        .collect();
    roots.push(PathBuf::from("/opt/ms-playwright"));
    if let Some(home) = dirs::home_dir() {
        roots.push(home.join(".cache/ms-playwright"));
    }
    roots
}

/// The Chromium to print with: the one named, else the newest headless
/// shell, else the newest full Chromium, under the Playwright roots.
/// Refused naming every place it looked, never a silent fallback.
pub fn find_chromium(named: Option<&Path>) -> Result<PathBuf, String> {
    if let Some(p) = named {
        return if p.is_file() {
            Ok(p.to_path_buf())
        } else {
            Err(format!("--chromium {}: no such file", p.display()))
        };
    }
    let roots = browser_roots();
    let kinds = [
        (
            "chromium_headless_shell-",
            "chrome-headless-shell-linux64/chrome-headless-shell",
        ),
        ("chromium-", "chrome-linux64/chrome"),
        ("chromium-", "chrome-linux/chrome"),
    ];
    for (prefix, binary) in kinds {
        for root in &roots {
            let Ok(entries) = std::fs::read_dir(root) else {
                continue;
            };
            // Newest revision first: the directory's number is Playwright's
            // browser revision.
            let mut found: Vec<(u64, PathBuf)> = entries
                .filter_map(|e| e.ok())
                .filter_map(|e| {
                    let name = e.file_name().to_string_lossy().into_owned();
                    let rev = name.strip_prefix(prefix)?.parse::<u64>().ok()?;
                    Some((rev, e.path().join(binary)))
                })
                .filter(|(_, p)| p.is_file())
                .collect();
            found.sort();
            if let Some((_, p)) = found.pop() {
                return Ok(p);
            }
        }
    }
    Err(format!(
        "no Chromium to print with: looked for chromium_headless_shell-*/ and chromium-*/ under {} — run where the boss-ci image's browser is (the dev pod, a boss-ci container) or name one with --chromium",
        roots
            .iter()
            .map(|r| r.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

/// Print `html` to PDF bytes with `chromium`. The page is written to a
/// private temporary directory with the browser's profile beside it, so
/// nothing is read from or left in the caller's home. On failure the
/// browser's whole output is carried in the error: this path is taken
/// when something is already wrong, and a tail is where the cause is not.
pub fn print(chromium: &Path, html: &str) -> Result<Vec<u8>, String> {
    let dir = tempfile::tempdir().map_err(|e| format!("a temporary directory: {e}"))?;
    let page = dir.path().join("sheet.html");
    let out = dir.path().join("sheet.pdf");
    std::fs::write(&page, html).map_err(|e| format!("{}: {e}", page.display()))?;
    // The browser's output goes to a file, not a pipe: a pipe nobody
    // drains until exit can fill and stall the print it is reporting on.
    let log_path = dir.path().join("chromium.log");
    let log =
        std::fs::File::create(&log_path).map_err(|e| format!("{}: {e}", log_path.display()))?;
    let log_err = log
        .try_clone()
        .map_err(|e| format!("{}: {e}", log_path.display()))?;
    let mut child = Command::new(chromium)
        .args([
            "--headless",
            // As Playwright launches it in a container: the sandbox needs
            // user namespaces the gate's pod does not grant.
            "--no-sandbox",
            "--disable-gpu",
            // The page carries its own footer (version, tree, time); the
            // browser's default header and footer would add the temporary
            // file's URL to the paper.
            "--no-pdf-header-footer",
        ])
        .arg(format!(
            "--user-data-dir={}",
            dir.path().join("profile").display()
        ))
        .arg(format!("--print-to-pdf={}", out.display()))
        .arg(file_url(&page))
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_err))
        // Its own process group, so the deadline reaches the zygote and
        // renderers Chromium forks, not only the browser process.
        .process_group(0)
        .spawn()
        .map_err(|e| format!("{}: {e}", chromium.display()))?;
    // Signalled only while the leader is alive and unreaped, so the group
    // id cannot have been reused by anything else.
    let group = format!("-{}", child.id());
    let kill_group = || {
        let _ = Command::new("kill")
            .args(["-KILL", "--", &group])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    };
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() > PRINT_DEADLINE => {
                kill_group();
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "{} did not finish printing in {}s — killed",
                    chromium.display(),
                    PRINT_DEADLINE.as_secs()
                ));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => return Err(format!("{}: {e}", chromium.display())),
        }
    };
    let bytes = std::fs::read(&out).unwrap_or_default();
    if !status.success() || !bytes.starts_with(b"%PDF-") {
        let said = std::fs::read_to_string(&log_path).unwrap_or_default();
        return Err(format!(
            "{} did not print a PDF ({status}); it said:\n{said}",
            chromium.display()
        ));
    }
    Ok(bytes)
}

/// `file://` + the absolute path, with the bytes a URL cannot carry
/// escaped (a temporary directory may hold a space).
fn file_url(path: &Path) -> String {
    use percent_encoding::{AsciiSet, CONTROLS, utf8_percent_encode};
    const PATH: &AsciiSet = &CONTROLS
        .add(b' ')
        .add(b'"')
        .add(b'#')
        .add(b'%')
        .add(b'?')
        .add(b'<')
        .add(b'>');
    format!(
        "file://{}",
        utf8_percent_encode(&path.to_string_lossy(), PATH)
    )
}

// ----- reading the text back ------------------------------------------

struct Obj {
    dict: String,
    stream: Option<Vec<u8>>,
}

fn bytes_re(re: &str) -> Option<BytesRegex> {
    BytesRegex::new(re).ok()
}

fn str_re(re: &str) -> Option<regex::Regex> {
    regex::Regex::new(re).ok()
}

const UNCOMPILED: &str = "the PDF reader's own patterns did not compile";

/// Every numbered object, its dictionary as text and its stream
/// inflated. Walked in order, skipping each stream's bytes by its
/// declared length, so a stream's binary content is never mistaken for
/// the start of an object.
fn objects(pdf: &[u8]) -> Result<BTreeMap<u32, Obj>, String> {
    static START: LazyLock<Option<BytesRegex>> =
        LazyLock::new(|| bytes_re(r"(?-u)(\d+)\s+\d+\s+obj\b"));
    static STREAM: LazyLock<Option<BytesRegex>> =
        LazyLock::new(|| bytes_re(r"(?-u)(?:endobj|>>\s*stream\r?\n)"));
    static LENGTH: LazyLock<Option<regex::Regex>> =
        LazyLock::new(|| str_re(r"/Length\s+(\d+)(\s+\d+\s+R)?"));
    let (Some(start), Some(stream), Some(length)) =
        (START.as_ref(), STREAM.as_ref(), LENGTH.as_ref())
    else {
        return Err(UNCOMPILED.into());
    };
    let mut objs = BTreeMap::new();
    let mut at = 0;
    while let Some(c) = start.captures_at(pdf, at) {
        let (Some(whole), Some(num)) = (c.get(0), c.get(1)) else {
            break;
        };
        let num: u32 = String::from_utf8_lossy(num.as_bytes())
            .parse()
            .map_err(|e| format!("object number: {e}"))?;
        let Some(end) = stream.find_at(pdf, whole.end()) else {
            return Err(format!("object {num} never ends"));
        };
        if end.as_bytes().starts_with(b"endobj") {
            objs.insert(
                num,
                Obj {
                    dict: String::from_utf8_lossy(&pdf[whole.end()..end.start()]).into_owned(),
                    stream: None,
                },
            );
            at = end.end();
            continue;
        }
        let dict = String::from_utf8_lossy(&pdf[whole.end()..end.start() + 2]).into_owned();
        let len = match length.captures(&dict) {
            Some(l) if l.get(2).is_none() => l
                .get(1)
                .and_then(|n| n.as_str().parse::<usize>().ok())
                .ok_or_else(|| format!("object {num}: a stream length that is not a number"))?,
            Some(_) => {
                return Err(format!(
                    "object {num}: an indirect stream length, which this printer does not write"
                ));
            }
            None => return Err(format!("object {num}: a stream with no length")),
        };
        let stop = end
            .end()
            .checked_add(len)
            .ok_or_else(|| format!("object {num}: a stream length past any file"))?;
        let data = pdf
            .get(end.end()..stop)
            .ok_or_else(|| format!("object {num}: its stream runs past the end of the file"))?;
        let data = if dict.contains("/FlateDecode") {
            let mut out = Vec::new();
            flate2::read::ZlibDecoder::new(data)
                .take(INFLATE_CAP + 1)
                .read_to_end(&mut out)
                .map_err(|e| format!("object {num}: its stream does not inflate: {e}"))?;
            if out.len() as u64 > INFLATE_CAP {
                return Err(format!(
                    "object {num}: its stream inflates past {INFLATE_CAP} bytes, which no page of this sheet does"
                ));
            }
            out
        } else {
            data.to_vec()
        };
        if dict.contains("/ObjStm") {
            return Err(format!(
                "object {num} is an object stream, which this printer does not write"
            ));
        }
        objs.insert(
            num,
            Obj {
                dict,
                stream: Some(data),
            },
        );
        at = stop;
    }
    if objs.is_empty() {
        return Err("no objects — this is not a PDF".into());
    }
    Ok(objs)
}

/// The first `/<key> N 0 R` in a dictionary.
fn reference(dict: &str, key: &str) -> Option<u32> {
    let re = regex::Regex::new(&format!(r"/{key}\s+(\d+)\s+\d+\s+R")).ok()?;
    re.captures(dict)?.get(1)?.as_str().parse().ok()
}

/// Every `N 0 R` in a text.
fn references(text: &str) -> Vec<u32> {
    static REF: LazyLock<Option<regex::Regex>> = LazyLock::new(|| str_re(r"(\d+)\s+\d+\s+R"));
    REF.as_ref()
        .map(|re| {
            re.captures_iter(text)
                .filter_map(|c| c.get(1)?.as_str().parse().ok())
                .collect()
        })
        .unwrap_or_default()
}

/// The `<< … >>` that follows `/<key>` in a dictionary, one level deep —
/// or, if the key names an object, that object's dictionary.
fn subdict(objs: &BTreeMap<u32, Obj>, dict: &str, key: &str) -> Option<String> {
    let at = dict.find(&format!("/{key}"))? + key.len() + 1;
    let rest = dict[at..].trim_start();
    if let Some(inner) = rest.strip_prefix("<<") {
        let mut depth = 1usize;
        let mut i = 0;
        let b = inner.as_bytes();
        while i + 1 < b.len() {
            match &b[i..i + 2] {
                b"<<" => {
                    depth += 1;
                    i += 2;
                }
                b">>" => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(inner[..i].to_string());
                    }
                    i += 2;
                }
                _ => i += 1,
            }
        }
        None
    } else {
        let n = references(rest.split('/').next().unwrap_or_default());
        n.first().and_then(|n| objs.get(n)).map(|o| o.dict.clone())
    }
}

/// The pages, in the order the page tree gives them.
fn pages(objs: &BTreeMap<u32, Obj>, pdf: &[u8]) -> Result<Vec<u32>, String> {
    static ROOT: LazyLock<Option<BytesRegex>> =
        LazyLock::new(|| bytes_re(r"(?-u)/Root\s+(\d+)\s+\d+\s+R"));
    let root = ROOT
        .as_ref()
        .ok_or(UNCOMPILED)?
        .captures_iter(pdf)
        .last()
        .and_then(|c| {
            String::from_utf8_lossy(c.get(1)?.as_bytes())
                .parse::<u32>()
                .ok()
        })
        .ok_or("no /Root in the trailer")?;
    let tree = objs
        .get(&root)
        .and_then(|o| reference(&o.dict, "Pages"))
        .ok_or("the catalog names no /Pages")?;
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    let mut todo = vec![tree];
    while let Some(n) = todo.pop() {
        if !seen.insert(n) {
            return Err(format!("the page tree visits object {n} twice"));
        }
        let o = objs
            .get(&n)
            .ok_or_else(|| format!("the page tree names object {n}, which is not in the file"))?;
        if o.dict.contains("/Kids") {
            let kids = o
                .dict
                .split_once("/Kids")
                .and_then(|(_, r)| r.split_once('['))
                .and_then(|(_, r)| r.split_once(']'))
                .map(|(k, _)| references(k))
                .unwrap_or_default();
            // Depth first, in order: push reversed so the first kid pops first.
            todo.extend(kids.into_iter().rev());
        } else {
            out.push(n);
        }
    }
    Ok(out)
}

/// A font's ToUnicode map: its code width in bytes and code -> text.
struct CMap {
    width: usize,
    map: BTreeMap<u32, String>,
}

fn hex_bytes(h: &str) -> Vec<u8> {
    let h: Vec<u8> = h.bytes().filter(|b| b.is_ascii_hexdigit()).collect();
    h.chunks(2)
        .map(|p| {
            let s = std::str::from_utf8(p).unwrap_or("0");
            // An odd trailing digit is followed by an implied 0 (PDF 7.3.4.3).
            u8::from_str_radix(&format!("{s:0<2}"), 16).unwrap_or(0)
        })
        .collect()
}

fn utf16(bytes: &[u8]) -> Vec<u16> {
    bytes
        .chunks(2)
        .map(|p| u16::from_be_bytes([p[0], *p.get(1).unwrap_or(&0)]))
        .collect()
}

fn code(bytes: &[u8]) -> u32 {
    bytes.iter().fold(0u32, |a, b| (a << 8) | u32::from(*b))
}

fn cmap(text: &str) -> Result<CMap, String> {
    static SPACE: LazyLock<Option<regex::Regex>> =
        LazyLock::new(|| str_re(r"begincodespacerange\s*<([0-9A-Fa-f]+)>"));
    static CHARS: LazyLock<Option<regex::Regex>> =
        LazyLock::new(|| str_re(r"(?s)beginbfchar(.*?)endbfchar"));
    static RANGES: LazyLock<Option<regex::Regex>> =
        LazyLock::new(|| str_re(r"(?s)beginbfrange(.*?)endbfrange"));
    static PAIR: LazyLock<Option<regex::Regex>> =
        LazyLock::new(|| str_re(r"<([0-9A-Fa-f]+)>\s*<([0-9A-Fa-f]*)>"));
    static RANGE: LazyLock<Option<regex::Regex>> = LazyLock::new(|| {
        str_re(r"<([0-9A-Fa-f]+)>\s*<([0-9A-Fa-f]+)>\s*(?:<([0-9A-Fa-f]*)>|\[([^\]]*)\])")
    });
    let (Some(space), Some(chars), Some(ranges), Some(pair), Some(range)) = (
        SPACE.as_ref(),
        CHARS.as_ref(),
        RANGES.as_ref(),
        PAIR.as_ref(),
        RANGE.as_ref(),
    ) else {
        return Err(UNCOMPILED.into());
    };
    let width = space
        .captures(text)
        .and_then(|c| c.get(1))
        .map(|m| hex_bytes(m.as_str()).len())
        .filter(|w| (1..=4).contains(w))
        .ok_or("a ToUnicode map with no code space")?;
    let mut map = BTreeMap::new();
    for block in chars.captures_iter(text).filter_map(|c| c.get(1)) {
        for p in pair.captures_iter(block.as_str()) {
            let (Some(src), Some(dst)) = (p.get(1), p.get(2)) else {
                continue;
            };
            map.insert(
                code(&hex_bytes(src.as_str())),
                String::from_utf16_lossy(&utf16(&hex_bytes(dst.as_str()))),
            );
        }
    }
    for block in ranges.captures_iter(text).filter_map(|c| c.get(1)) {
        for r in range.captures_iter(block.as_str()) {
            let (Some(lo), Some(hi)) = (r.get(1), r.get(2)) else {
                continue;
            };
            let (lo, hi) = (code(&hex_bytes(lo.as_str())), code(&hex_bytes(hi.as_str())));
            if hi < lo || hi - lo > 0xFFFF {
                return Err("a ToUnicode range that runs backwards or past a plane".into());
            }
            if let Some(dst) = r.get(3) {
                // One destination: the last UTF-16 unit counts up with the code.
                let base = utf16(&hex_bytes(dst.as_str()));
                for (i, c) in (lo..=hi).enumerate() {
                    let mut units = base.clone();
                    if let Some(last) = units.last_mut() {
                        *last = last.wrapping_add(i as u16);
                    }
                    map.insert(c, String::from_utf16_lossy(&units));
                }
            } else if let Some(list) = r.get(4) {
                let dsts = hex_strings(list.as_str());
                for (c, d) in (lo..=hi).zip(dsts) {
                    map.insert(c, String::from_utf16_lossy(&utf16(&d)));
                }
            }
        }
    }
    Ok(CMap { width, map })
}

fn hex_strings(text: &str) -> Vec<Vec<u8>> {
    text.split('<')
        .skip(1)
        .filter_map(|s| s.split_once('>').map(|(h, _)| hex_bytes(h)))
        .collect()
}

/// One lexed piece of a content stream.
enum Tok {
    Str(Vec<u8>),
    Array(Vec<Vec<u8>>),
    Name(String),
    Num(f64),
    Op(String),
}

/// A literal string `( … )`, from just after its `(`: its bytes and
/// where it ends. Escapes per PDF 7.3.4.2; balanced parentheses nest.
fn literal(b: &[u8], mut i: usize) -> (Vec<u8>, usize) {
    let mut out = Vec::new();
    let mut depth = 1usize;
    while i < b.len() {
        match b[i] {
            b'\\' if i + 1 < b.len() => {
                i += 1;
                match b[i] {
                    b'n' => out.push(b'\n'),
                    b'r' => out.push(b'\r'),
                    b't' => out.push(b'\t'),
                    b'b' => out.push(8),
                    b'f' => out.push(12),
                    d @ b'0'..=b'7' => {
                        let mut v = u32::from(d - b'0');
                        let mut n = 1;
                        while n < 3 && i + 1 < b.len() && (b'0'..=b'7').contains(&b[i + 1]) {
                            i += 1;
                            n += 1;
                            v = v * 8 + u32::from(b[i] - b'0');
                        }
                        out.push((v & 0xFF) as u8);
                    }
                    b'\n' | b'\r' => {}
                    other => out.push(other),
                }
            }
            b'(' => {
                depth += 1;
                out.push(b'(');
            }
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return (out, i + 1);
                }
                out.push(b')');
            }
            c => out.push(c),
        }
        i += 1;
    }
    (out, i)
}

fn is_delim(c: u8) -> bool {
    c.is_ascii_whitespace() || b"()<>[]{}/%".contains(&c)
}

fn lex(b: &[u8]) -> Vec<Tok> {
    let mut toks = Vec::new();
    let mut array: Option<Vec<Vec<u8>>> = None;
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c.is_ascii_whitespace() {
            i += 1;
        } else if c == b'%' {
            while i < b.len() && b[i] != b'\n' && b[i] != b'\r' {
                i += 1;
            }
        } else if b[i..].starts_with(b"<<") || b[i..].starts_with(b">>") {
            i += 2;
        } else if c == b'<' {
            let end = b[i..]
                .iter()
                .position(|&x| x == b'>')
                .map_or(b.len(), |p| i + p);
            let s = hex_bytes(&String::from_utf8_lossy(&b[i + 1..end]));
            match array.as_mut() {
                Some(a) => a.push(s),
                None => toks.push(Tok::Str(s)),
            }
            i = end + 1;
        } else if c == b'(' {
            let (s, end) = literal(b, i + 1);
            match array.as_mut() {
                Some(a) => a.push(s),
                None => toks.push(Tok::Str(s)),
            }
            i = end;
        } else if c == b'[' {
            array = Some(Vec::new());
            i += 1;
        } else if c == b']' {
            if let Some(a) = array.take() {
                toks.push(Tok::Array(a));
            }
            i += 1;
        } else {
            let start = i;
            i += 1;
            while i < b.len() && !is_delim(b[i]) {
                i += 1;
            }
            let word = String::from_utf8_lossy(&b[start..i]).into_owned();
            if array.is_some() {
                // A number inside a TJ array is a kerning adjustment.
                continue;
            }
            toks.push(if let Some(name) = word.strip_prefix('/') {
                Tok::Name(name.to_string())
            } else if let Ok(n) = word.parse::<f64>() {
                Tok::Num(n)
            } else {
                Tok::Op(word)
            });
        }
    }
    toks
}

/// One piece of shown text and where it sits: the page (from 0) and the
/// baseline's height above the page's bottom edge, in points — the
/// origin of the text rendering matrix, through every `cm` in force.
#[derive(Debug, Clone, PartialEq)]
pub struct Run {
    pub page: usize,
    pub y: f64,
    pub text: String,
}

/// A PDF matrix `[a b c d e f]`; `mul(m, n)` is `m × n`, the PDF order in
/// which `m` applies first.
type Matrix = [f64; 6];
const IDENTITY: Matrix = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];

fn mul(m: &Matrix, n: &Matrix) -> Matrix {
    [
        m[0] * n[0] + m[1] * n[2],
        m[0] * n[1] + m[1] * n[3],
        m[2] * n[0] + m[3] * n[2],
        m[2] * n[1] + m[3] * n[3],
        m[4] * n[0] + m[5] * n[2] + n[4],
        m[4] * n[1] + m[5] * n[3] + n[5],
    ]
}

/// The last `N` numeric operands, in order.
fn numbers<const N: usize>(operands: &[Tok]) -> Option<[f64; N]> {
    let nums: Vec<f64> = operands
        .iter()
        .filter_map(|t| match t {
            Tok::Num(n) => Some(*n),
            _ => None,
        })
        .collect();
    nums.get(nums.len().checked_sub(N)?..)?.try_into().ok()
}

/// Runs joined into text: a line break wherever the page or the
/// baseline changes.
pub fn join(runs: &[Run]) -> String {
    let mut out = String::new();
    let mut at: Option<(usize, f64)> = None;
    for r in runs {
        if let Some((page, y)) = at
            && (page != r.page || (y - r.y).abs() > 1.0)
        {
            out.push('\n');
        }
        out.push_str(&r.text);
        at = Some((r.page, r.y));
    }
    out.push('\n');
    out
}

/// Every piece of text a Chromium-printed PDF shows, page by page in
/// content order, each decoded through its font's ToUnicode map and
/// placed on its page (review of car 2, F1: the words being in the file
/// did not prove they could be read on the paper — the footer printed
/// over them).
pub fn runs(pdf: &[u8]) -> Result<Vec<Run>, String> {
    let objs = objects(pdf)?;
    let mut maps: BTreeMap<u32, CMap> = BTreeMap::new();
    let mut out: Vec<Run> = Vec::new();
    for (index, page) in pages(&objs, pdf)?.into_iter().enumerate() {
        let dict = &objs
            .get(&page)
            .ok_or_else(|| format!("page object {page} is not in the file"))?
            .dict;
        let resources = subdict(&objs, dict, "Resources")
            .ok_or_else(|| format!("page object {page} has no /Resources"))?;
        let fonts: BTreeMap<String, u32> = subdict(&objs, &resources, "Font")
            .map(|f| {
                f.split('/')
                    .skip(1)
                    .filter_map(|e| {
                        let (name, rest) = e.split_once(char::is_whitespace)?;
                        Some((name.to_string(), *references(rest).first()?))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let contents = match reference(dict, "Contents") {
            Some(n) => vec![n],
            None => dict
                .split_once("/Contents")
                .and_then(|(_, r)| r.split_once('['))
                .and_then(|(_, r)| r.split_once(']'))
                .map(|(k, _)| references(k))
                .unwrap_or_default(),
        };
        let mut font: Option<String> = None;
        let mut ctm = IDENTITY;
        let mut saved: Vec<Matrix> = Vec::new();
        let (mut tm, mut tlm) = (IDENTITY, IDENTITY);
        for n in contents {
            let stream = objs
                .get(&n)
                .and_then(|o| o.stream.as_ref())
                .ok_or_else(|| format!("page object {page}'s content {n} is not a stream"))?;
            let mut operands: Vec<Tok> = Vec::new();
            for tok in lex(stream) {
                let Tok::Op(op) = tok else {
                    operands.push(tok);
                    continue;
                };
                let shown: Vec<Vec<u8>> = match op.as_str() {
                    "Tf" => {
                        font = operands.iter().rev().find_map(|t| match t {
                            Tok::Name(n) => Some(n.clone()),
                            _ => None,
                        });
                        vec![]
                    }
                    "Tj" | "'" | "\"" => operands
                        .iter()
                        .rev()
                        .find_map(|t| match t {
                            Tok::Str(s) => Some(vec![s.clone()]),
                            _ => None,
                        })
                        .unwrap_or_default(),
                    "TJ" => operands
                        .iter()
                        .rev()
                        .find_map(|t| match t {
                            Tok::Array(a) => Some(a.clone()),
                            _ => None,
                        })
                        .unwrap_or_default(),
                    _ => vec![],
                };
                match op.as_str() {
                    "q" => saved.push(ctm),
                    "Q" => ctm = saved.pop().unwrap_or(IDENTITY),
                    "cm" => {
                        if let Some(m) = numbers::<6>(&operands) {
                            ctm = mul(&m, &ctm);
                        }
                    }
                    "BT" => (tm, tlm) = (IDENTITY, IDENTITY),
                    "Tm" => {
                        if let Some(m) = numbers::<6>(&operands) {
                            (tm, tlm) = (m, m);
                        }
                    }
                    "Td" | "TD" => {
                        if let Some([tx, ty]) = numbers::<2>(&operands) {
                            tlm = mul(&[1.0, 0.0, 0.0, 1.0, tx, ty], &tlm);
                            tm = tlm;
                        }
                    }
                    _ => {}
                }
                if !shown.is_empty() {
                    let y = mul(&tm, &ctm)[5];
                    let mut text = String::new();
                    let name = font.clone().unwrap_or_default();
                    let obj = *fonts.get(&name).ok_or_else(|| {
                        format!("page object {page} shows text in font /{name}, which its resources do not name")
                    })?;
                    let map = match maps.entry(obj) {
                        std::collections::btree_map::Entry::Occupied(e) => e.into_mut(),
                        std::collections::btree_map::Entry::Vacant(v) => {
                            let to_unicode = objs
                                .get(&obj)
                                .and_then(|o| reference(&o.dict, "ToUnicode"))
                                .and_then(|t| objs.get(&t))
                                .and_then(|o| o.stream.as_ref())
                                .ok_or_else(|| {
                                    format!("font /{name} (object {obj}) has no ToUnicode map, so its text cannot be read")
                                })?;
                            v.insert(cmap(&String::from_utf8_lossy(to_unicode))?)
                        }
                    };
                    for s in shown {
                        for c in s.chunks(map.width) {
                            match map.map.get(&code(c)) {
                                Some(t) => text.push_str(t),
                                None => text.push('\u{FFFD}'),
                            }
                        }
                    }
                    out.push(Run {
                        page: index,
                        y,
                        text,
                    });
                }
                operands.clear();
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A ToUnicode map in the three shapes the spec allows: single
    /// codes, a range counting up from one destination, and a range
    /// naming each destination — including one that decodes to two
    /// characters (a ligature).
    #[test]
    fn a_to_unicode_map_reads_every_shape() {
        let m = cmap(
            "1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n\
             2 beginbfchar\n<0003> <0020>\n<020B> <2014>\nendbfchar\n\
             2 beginbfrange\n<0044> <0046> <0061>\n<0100> <0101> [<00660069> <0041>]\nendbfrange\n",
        )
        .unwrap();
        assert_eq!(m.width, 2);
        let read = |codes: &[u32]| codes.iter().map(|c| m.map[c].clone()).collect::<String>();
        assert_eq!(read(&[0x44, 0x45, 0x46, 0x03, 0x20B]), "abc \u{2014}");
        assert_eq!(read(&[0x100, 0x101]), "fiA");
    }

    /// A content stream's shows — a hex `Tj`, a `TJ` array with kerning
    /// numbers, a literal string with an escape — decoded in order.
    #[test]
    fn a_content_stream_is_read_in_order() {
        let toks =
            lex(b"BT /F4 12 Tf 1 0 0 -1 5 5 Tm <0044> Tj [<0045> -12 <0046>] TJ (a\\)b) Tj ET");
        let shown: Vec<String> = toks
            .iter()
            .filter_map(|t| match t {
                Tok::Str(s) => Some(String::from_utf8_lossy(s).into_owned()),
                Tok::Array(a) => Some(a.iter().map(|s| format!("{s:?}")).collect()),
                _ => None,
            })
            .collect();
        assert_eq!(shown, ["\0D", "[0, 69][0, 70]", "a)b"]);
    }

    #[test]
    fn not_a_pdf_is_refused_not_read_as_empty() {
        assert!(runs(b"hello").is_err());
    }

    /// One FlateDecode object whose stream inflates to `inflated` zero
    /// bytes — written in chunks, so the test never holds the inflated
    /// size, only the ~1000:1 compressed one.
    fn flate_object(inflated: u64) -> Vec<u8> {
        use std::io::Write;
        let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
        let chunk = vec![0u8; 1 << 20];
        let mut left = inflated;
        while left > 0 {
            let n = left.min(chunk.len() as u64) as usize;
            z.write_all(&chunk[..n]).unwrap();
            left -= n as u64;
        }
        let data = z.finish().unwrap();
        let mut pdf = format!(
            "%PDF-1.4\n1 0 obj\n<< /Length {} /Filter /FlateDecode >>\nstream\n",
            data.len()
        )
        .into_bytes();
        pdf.extend_from_slice(&data);
        pdf.extend_from_slice(b"\nendstream\nendobj\n%%EOF\n");
        pdf
    }

    /// A ZLIB BOMB is refused at INFLATE_CAP, not inflated into memory
    /// (review of car 2, the NIT on run 4533d570). The reader inflates
    /// every FlateDecode stream of a PDF it is handed; a stream a few
    /// dozen kilobytes long can inflate a thousand times over, and the
    /// cap is what stops a hostile or broken file from taking the
    /// process's memory with it. The bomb is one byte past the cap — the
    /// boundary itself — and the control, a stream exactly AT the cap,
    /// proves the fixture inflates at all, so the refusal is the cap's
    /// and not a malformed stream's.
    #[test]
    fn a_zlib_bomb_is_refused_at_the_inflate_cap() {
        let bomb = flate_object(INFLATE_CAP + 1);
        assert!(
            (bomb.len() as u64) < INFLATE_CAP / 256,
            "the bomb is small on disk: {} bytes",
            bomb.len()
        );
        let Err(refused) = objects(&bomb) else {
            panic!("a stream past the cap is refused, not inflated");
        };
        assert!(
            refused.contains("inflates past") && refused.contains(&INFLATE_CAP.to_string()),
            "{refused}"
        );

        let at_cap = objects(&flate_object(INFLATE_CAP)).expect("a stream at the cap inflates");
        let stream = at_cap[&1].stream.as_ref().expect("the object has a stream");
        assert_eq!(stream.len() as u64, INFLATE_CAP);
        assert!(stream.iter().all(|b| *b == 0));
    }
}
