//! tree-wide pin — the schematic-lives-once check walks every tracked file
//!
//! `infra/forge/netboot.sh` and `infra/forge/netboot-http.py` — network
//! boot from the forge (backlog ee31ab73, David 2026-10-19: w-2 was
//! reinstalled on a new disk and looped on PXE for want of boot media,
//! and nothing on the LAN answered PXE).
//!
//! What the tree can prove, and what it cannot. Every leg here runs
//! against fixtures and stubs: the declaration read from its one file,
//! the mirror keeping only bytes whose sha256 the declaration names, the
//! dnsmasq configuration rendered proxy-only (no lease, no DNS), the iPXE
//! script rendered from the declaration and the Image Factory's kernel
//! args, the server refusing to start on bytes it cannot verify and
//! serving nothing but its three names, and `ensure` converging a
//! scratch forge. That a real PXE ROM on the LAN reaches the Talos
//! maintenance console is the host's proof (join 829f75a8's measure
//! step); the car's recorded probe asks the converged forge for the
//! script and the bytes.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use boss_testing::{create_dir, feed_stdin, repo_root, scratch_dir, write_exec, write_file};

const DECLARATION: &str = "infra/cluster/talos/netboot.toml";

/// A fixture pair: never the real schematic, which lives in one file.
const FX_SCHEMATIC: &str = "abababababababababababababababababababababababababababababababab";
const FX_VERSION: &str = "v0.0.1";
const FX_ARGS: &str = "talos.platform=metal console=tty0 init_on_alloc=1";
const FX_KERNEL: &str = "fixture kernel bytes\n";
const FX_INITRAMFS: &str = "fixture initramfs bytes\n";
/// TEST-NET-1 (RFC 5737): never an estate address.
const FX_ADDR: &str = "192.0.2.15";

fn script() -> PathBuf {
    repo_root().join("infra/forge/netboot.sh")
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

fn read(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

fn sha256(body: &[u8]) -> String {
    let mut c = Command::new("sha256sum")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("sha256sum");
    feed_stdin(&mut c, body);
    let o = c.wait_with_output().unwrap();
    assert!(o.status.success(), "sha256sum exited {:?}", o.status);
    String::from_utf8_lossy(&o.stdout)
        .split_whitespace()
        .next()
        .unwrap()
        .to_string()
}

/// `netboot.sh <args>` with every seam the caller does not name removed,
/// so nothing of the host's leaks into a leg.
fn netboot(args: &[&str], env: &[(&str, &Path)]) -> Command {
    let mut c = Command::new("bash");
    c.arg(script()).args(args);
    for k in [
        "NETBOOT_DECLARATION",
        "NETBOOT_FACTORY_URL",
        "NETBOOT_IPXE_DIR",
        "NETBOOT_DNSMASQ",
        "INSTALL_ETC",
        "INSTALL_SYSTEMCTL",
        "INSTALL_APT_GET",
        "INSTALL_NETBOOT_ROOT",
        "BOSS_SOR_ENV",
        "BOSS_FORGE_HOST",
        "BOSS_FIRST_INSTALL_AS",
        "BOSS_HOST_CHECK_ESTATE",
        "BOSS_RUN_SUMMARY_FILE",
    ] {
        c.env_remove(k);
    }
    for (k, v) in env {
        c.env(k, v);
    }
    c
}

/// `key = "value"` from the one declaration, read here independently of
/// the script so the test does not grade the script by itself.
fn declared(key: &str) -> String {
    let body = read(&repo_root().join(DECLARATION));
    let hits: Vec<&str> = body
        .lines()
        .filter_map(|l| l.strip_prefix(&format!("{key} = \"")))
        .filter_map(|l| l.strip_suffix('"'))
        .collect();
    assert_eq!(hits.len(), 1, "{DECLARATION} declares {key} once: {hits:?}");
    hits[0].to_string()
}

fn write_declaration(dir: &Path, kernel_sha: &str, initramfs_sha: &str, args: &str) -> PathBuf {
    write_declaration_for(dir, kernel_sha, initramfs_sha, args, "")
}

/// A fixture declaration that answers `macs` (space-separated) and nobody
/// else.
fn write_declaration_for(
    dir: &Path,
    kernel_sha: &str,
    initramfs_sha: &str,
    args: &str,
    macs: &str,
) -> PathBuf {
    let p = dir.join("netboot.toml");
    write_file(
        &p,
        &format!(
            "# fixture\nschematic = \"{FX_SCHEMATIC}\"\nversion = \"{FX_VERSION}\"\n\
             kernel_sha256 = \"{kernel_sha}\"\ninitramfs_sha256 = \"{initramfs_sha}\"\n\
             kernel_args = \"{args}\"\nnetboot_macs = \"{macs}\"\n"
        ),
    );
    p
}

/// Two fixture NICs from the locally-administered range (the 02: bit set),
/// so neither is a real vendor's address; and one that is never declared.
const FX_MAC_A: &str = "02:00:5e:10:00:01";
const FX_MAC_B: &str = "02:00:5e:10:00:02";
const FX_MAC_UNDECLARED: &str = "02:00:5e:10:00:99";

fn declaration_for(dir: &Path, macs: &str) -> PathBuf {
    write_declaration_for(
        dir,
        &sha256(FX_KERNEL.as_bytes()),
        &sha256(FX_INITRAMFS.as_bytes()),
        FX_ARGS,
        macs,
    )
}

/// The directives of a rendered dnsmasq config: comments and blanks out.
fn directives(conf: &str) -> Vec<&str> {
    conf.lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
        .collect()
}

/// Does dnsmasq answer a PXE client with this MAC? The rule is dnsmasq
/// 2.90's own, read from its source (src/rfc2131.c, dhcp_reply): every
/// `dhcp-mac=set:<tag>,<mac>` whose address equals the client's chaddr
/// adds <tag>; `dhcp-ignore=tag:<t>[,tag:<t>]` sets `ignore` when every
/// listed condition holds (`!t` = t absent); and BOTH PXE paths — the
/// proxy offer and the port-4011 boot-server ACK — return no packet at
/// all when `ignore` is set (the proxy one logs `proxy-ignored`). A
/// wildcard in a dhcp-mac is refused here rather than modelled: this
/// config never writes one.
fn dnsmasq_answers(conf: &str, mac: &str) -> bool {
    let lines = directives(conf);
    let mut tags: Vec<&str> = Vec::new();
    for v in lines.iter().filter_map(|l| l.strip_prefix("dhcp-mac=")) {
        let (tag, m) = v.split_once(',').expect("dhcp-mac=set:<tag>,<mac>");
        let tag = tag.strip_prefix("set:").expect("dhcp-mac sets a tag");
        assert!(!m.contains('*'), "no wildcard MAC is ever rendered: {v}");
        if m.eq_ignore_ascii_case(mac) {
            tags.push(tag);
        }
    }
    let ignored = lines
        .iter()
        .filter_map(|l| l.strip_prefix("dhcp-ignore="))
        .any(|v| {
            v.split(',').all(|t| {
                let t = t.strip_prefix("tag:").expect("dhcp-ignore=tag:<t>");
                match t.strip_prefix('!') {
                    Some(absent) => !tags.contains(&absent),
                    None => tags.contains(&t),
                }
            })
        });
    let proxy = lines
        .iter()
        .any(|l| l.starts_with("dhcp-range=") && l.ends_with(",proxy"));
    let served = lines.iter().any(|l| l.starts_with("pxe-service="));
    !ignored && proxy && served
}

/// Does the rendered iPXE script boot Talos on a NIC with this MAC, or
/// exit to the firmware's next boot device? Walks the script the way
/// iPXE does, over the only commands it may contain; any other command
/// panics, so a script that grows one is re-read here first. iPXE prints
/// `${netX/mac}` as lowercase colon-separated hex, which is why the
/// declaration admits only that spelling.
fn ipxe_boots(script: &str, mac: &str) -> bool {
    let lines: Vec<&str> = script
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && (!l.starts_with('#') || l.starts_with("#!")))
        .collect();
    assert_eq!(lines.first(), Some(&"#!ipxe"), "{script}");
    let label = |name: &str| {
        lines
            .iter()
            .position(|l| *l == format!(":{name}"))
            .unwrap_or_else(|| panic!("no :{name} label: {script}"))
    };
    let mut i = 1;
    loop {
        let l = *lines
            .get(i)
            .unwrap_or_else(|| panic!("ran off the end without boot or exit: {script}"));
        if let Some(rest) = l.strip_prefix("iseq ${netX/mac} ") {
            let (m, tail) = rest.split_once(' ').expect("iseq <a> <b> ...");
            assert_eq!(tail, "&& goto talos ||", "{l}");
            if m == mac {
                i = label("talos") + 1;
                continue;
            }
        } else if l == "exit" {
            return false;
        } else if l == "boot" {
            return true;
        } else {
            let cmd = l.split(' ').next().unwrap();
            assert!(
                ["echo", "imgfree", "kernel", "initrd"].contains(&cmd) || cmd.starts_with(':'),
                "a command this model does not know: {l}\n{script}"
            );
        }
        i += 1;
    }
}

/// A factory laid out as factory.talos.dev serves it, read by file://.
fn factory(dir: &Path) -> PathBuf {
    let root = dir.join("factory");
    let at = root.join(format!("image/{FX_SCHEMATIC}/{FX_VERSION}"));
    create_dir(&at);
    write_file(&at.join("kernel-amd64"), FX_KERNEL);
    write_file(&at.join("initramfs-amd64.xz"), FX_INITRAMFS);
    root
}

fn file_url(p: &Path) -> PathBuf {
    PathBuf::from(format!("file://{}", p.display()))
}

fn good_declaration(dir: &Path) -> PathBuf {
    write_declaration(
        dir,
        &sha256(FX_KERNEL.as_bytes()),
        &sha256(FX_INITRAMFS.as_bytes()),
        FX_ARGS,
    )
}

// ── the declaration ──────────────────────────────────────────────────

#[test]
fn the_declared_pair_is_read_from_the_one_declaration() {
    let o = netboot(&["declared"], &[]).output().unwrap();
    assert_eq!(o.status.code(), Some(0), "{}", text(&o));
    let out = String::from_utf8_lossy(&o.stdout).into_owned();
    for key in [
        "schematic",
        "version",
        "kernel_sha256",
        "initramfs_sha256",
        "kernel_args",
    ] {
        let want = format!("{key}={}", declared(key));
        assert!(
            out.lines().any(|l| l == want),
            "`declared` prints {want}: {out}"
        );
    }
    let schematic = declared("schematic");
    assert!(
        schematic.len() == 64 && schematic.bytes().all(|b| b.is_ascii_hexdigit()),
        "an Image Factory schematic id is 64 hex: {schematic}"
    );
    let version = declared("version");
    assert!(version.starts_with('v'), "a Talos version tag: {version}");
    // The metal args the factory's own PXE script carries (decision 2 on
    // the packet): the declaration copies them, so the platform is metal.
    let args = declared("kernel_args");
    for want in [
        "talos.platform=metal",
        "console=tty0",
        "init_on_alloc=1",
        "pti=on",
    ] {
        assert!(
            args.split(' ').any(|a| a == want),
            "kernel_args carries {want}: {args}"
        );
    }
}

/// CLAUDE.md §9a: the schematic is declared once. The probe and the
/// script read it from the declaration; no other tracked file spells it.
#[test]
fn the_schematic_lives_in_the_declaration_and_nowhere_else() {
    let root = repo_root();
    let schematic = declared("schematic");
    let o = Command::new("git")
        .arg("-c")
        .arg(format!("safe.directory={}", root.display()))
        .arg("-C")
        .arg(&root)
        .args(["ls-files", "-z"])
        .output()
        .unwrap();
    assert!(o.status.success(), "git ls-files: {}", text(&o));
    let files: Vec<String> = String::from_utf8_lossy(&o.stdout)
        .split('\0')
        .filter(|f| !f.is_empty())
        .map(str::to_string)
        .collect();
    assert!(files.len() > 1000, "too few tracked files: {}", files.len());
    let spelled: Vec<&String> = files
        .iter()
        .filter(|f| f.as_str() != DECLARATION)
        .filter(|f| {
            std::fs::read(root.join(f))
                .map(|b| {
                    b.windows(schematic.len())
                        .any(|w| w == schematic.as_bytes())
                })
                .unwrap_or(false)
        })
        .collect();
    assert!(
        spelled.is_empty(),
        "the declared schematic is spelled outside {DECLARATION}: {spelled:?} — read it from the \
         declaration (netboot.sh declared) instead"
    );
}

#[test]
fn a_declaration_that_could_carry_an_ipxe_command_is_refused() {
    let dir = scratch_dir("netboot-bad-declaration");
    let k = sha256(FX_KERNEL.as_bytes());
    let i = sha256(FX_INITRAMFS.as_bytes());
    for bad in [
        "talos.platform=metal ${net0/mac}",
        "talos.platform=metal && shell",
        "talos.platform=metal; shell",
        "console=tty0",
    ] {
        let decl = write_declaration(&dir, &k, &i, bad);
        let o = netboot(&["declared"], &[("NETBOOT_DECLARATION", &decl)])
            .output()
            .unwrap();
        assert_eq!(o.status.code(), Some(2), "{bad}: {}", text(&o));
        assert!(text(&o).contains("kernel_args"), "{bad}: {}", text(&o));
    }
    // A key missing is refused by name, never read as empty.
    let p = dir.join("short.toml");
    write_file(&p, &format!("schematic = \"{FX_SCHEMATIC}\"\n"));
    let o = netboot(&["declared"], &[("NETBOOT_DECLARATION", &p)])
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(2), "{}", text(&o));
    assert!(text(&o).contains("version"), "{}", text(&o));
}

// ── who is answered (review bcff47dd N1, backlog 8cf6faf7) ───────────

/// The allowlist is declared once, in the one declaration, and every
/// address in it is one iPXE and dnsmasq read the same way. Empty is a
/// legal value: it answers nobody.
#[test]
fn the_allowlist_is_declared_once_and_every_mac_is_well_formed() {
    let macs = declared("netboot_macs");
    for m in macs.split(' ').filter(|m| !m.is_empty()) {
        let parts: Vec<&str> = m.split(':').collect();
        assert!(
            parts.len() == 6
                && parts.iter().all(|p| p.len() == 2
                    && p.bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))),
            "{DECLARATION} netboot_macs holds {m}, not a lowercase colon-separated MAC"
        );
    }
    let o = netboot(&["declared"], &[]).output().unwrap();
    assert_eq!(o.status.code(), Some(0), "{}", text(&o));
    let out = String::from_utf8_lossy(&o.stdout).into_owned();
    let want = format!("netboot_macs={macs}");
    assert!(
        out.lines().any(|l| l == want),
        "`declared` prints {want}: {out}"
    );
}

#[test]
fn a_malformed_or_repeated_mac_is_refused() {
    let dir = scratch_dir("netboot-bad-mac");
    for bad in [
        // iPXE prints ${netX/mac} lowercase, so an uppercase entry would
        // pass dnsmasq and never match the script's iseq.
        "02:00:5E:10:00:01",
        "02-00-5e-10-00-01",
        "02:00:5e:10:00",
        "02:00:5e:10:00:*",
        "02:00:5e:10:00:01  02:00:5e:10:00:02",
        " 02:00:5e:10:00:01",
        "02:00:5e:10:00:01,02:00:5e:10:00:02",
        "02:00:5e:10:00:01 02:00:5e:10:00:01",
        "*",
    ] {
        let decl = declaration_for(&dir, bad);
        let o = netboot(&["declared"], &[("NETBOOT_DECLARATION", &decl)])
            .output()
            .unwrap();
        assert_eq!(o.status.code(), Some(2), "{bad:?}: {}", text(&o));
        assert!(text(&o).contains("netboot_macs"), "{bad:?}: {}", text(&o));
    }
    // A declaration that does not say who is answered is refused by name,
    // never read as "everyone".
    let p = dir.join("no-macs.toml");
    let body = read(&declaration_for(&dir, ""));
    write_file(
        &p,
        &body
            .lines()
            .filter(|l| !l.starts_with("netboot_macs"))
            .map(|l| format!("{l}\n"))
            .collect::<String>(),
    );
    let o = netboot(&["declared"], &[("NETBOOT_DECLARATION", &p)])
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(2), "{}", text(&o));
    assert!(text(&o).contains("netboot_macs"), "{}", text(&o));
}

fn render_dnsmasq_for(decl: &Path) -> String {
    let o = netboot(
        &[
            "render-dnsmasq",
            FX_ADDR,
            "/var/lib/boss/netboot/tftp",
            "http://192.0.2.15:8079/boot.ipxe",
        ],
        &[("NETBOOT_DECLARATION", decl)],
    )
    .output()
    .unwrap();
    assert_eq!(o.status.code(), Some(0), "{}", text(&o));
    String::from_utf8_lossy(&o.stdout).into_owned()
}

/// An installed node whose firmware tries the network before its disk
/// must get NO answer: dnsmasq stays silent, the firmware times out and
/// boots the disk. Only a declared MAC is answered.
#[test]
fn dnsmasq_answers_a_declared_mac_and_ignores_every_other() {
    let dir = scratch_dir("netboot-allowlist");
    let conf = render_dnsmasq_for(&declaration_for(&dir, &format!("{FX_MAC_A} {FX_MAC_B}")));
    assert!(dnsmasq_answers(&conf, FX_MAC_A), "{conf}");
    assert!(dnsmasq_answers(&conf, FX_MAC_B), "{conf}");
    assert!(!dnsmasq_answers(&conf, FX_MAC_UNDECLARED), "{conf}");
    let tagged: Vec<&str> = directives(&conf)
        .into_iter()
        .filter(|l| l.starts_with("dhcp-mac="))
        .collect();
    assert_eq!(
        tagged,
        vec![
            format!("dhcp-mac=set:netboot,{FX_MAC_A}").as_str(),
            format!("dhcp-mac=set:netboot,{FX_MAC_B}").as_str(),
        ],
        "one dhcp-mac per declared MAC and none other\n{conf}"
    );
}

#[test]
fn an_empty_allowlist_answers_nobody() {
    let dir = scratch_dir("netboot-allowlist-empty");
    let decl = declaration_for(&dir, "");
    let conf = render_dnsmasq_for(&decl);
    for mac in [FX_MAC_A, FX_MAC_B, FX_MAC_UNDECLARED] {
        assert!(!dnsmasq_answers(&conf, mac), "{mac}: {conf}");
    }
    let o = netboot(
        &["render-ipxe", "http://192.0.2.15:8079"],
        &[("NETBOOT_DECLARATION", &decl)],
    )
    .output()
    .unwrap();
    assert_eq!(o.status.code(), Some(0), "{}", text(&o));
    let script = String::from_utf8_lossy(&o.stdout).into_owned();
    for mac in [FX_MAC_A, FX_MAC_UNDECLARED] {
        assert!(!ipxe_boots(&script, mac), "{mac} exits to disk: {script}");
    }
}

/// The second fence, for an iPXE that reached the script some other way:
/// a NIC the declaration does not name exits to the firmware's next boot
/// device instead of booting Talos from RAM.
#[test]
fn the_ipxe_script_exits_to_disk_for_an_undeclared_mac() {
    let dir = scratch_dir("netboot-ipxe-allowlist");
    let decl = declaration_for(&dir, &format!("{FX_MAC_A} {FX_MAC_B}"));
    let o = netboot(
        &["render-ipxe", "http://192.0.2.15:8079"],
        &[("NETBOOT_DECLARATION", &decl)],
    )
    .output()
    .unwrap();
    assert_eq!(o.status.code(), Some(0), "{}", text(&o));
    let script = String::from_utf8_lossy(&o.stdout).into_owned();
    assert!(ipxe_boots(&script, FX_MAC_A), "{script}");
    assert!(ipxe_boots(&script, FX_MAC_B), "{script}");
    assert!(!ipxe_boots(&script, FX_MAC_UNDECLARED), "{script}");
}

// ── the mirror ───────────────────────────────────────────────────────

#[test]
fn the_mirror_keeps_only_bytes_whose_sha_is_declared() {
    let dir = scratch_dir("netboot-mirror");
    let decl = good_declaration(&dir);
    let fac = factory(&dir);
    let images = dir.join("images");
    let run = |fac: &Path| {
        netboot(
            &["mirror", images.to_str().unwrap()],
            &[
                ("NETBOOT_DECLARATION", &decl),
                ("NETBOOT_FACTORY_URL", &file_url(fac)),
            ],
        )
        .output()
        .unwrap()
    };
    let o = run(&fac);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o));
    let generation = images.join(format!("{FX_VERSION}-{FX_SCHEMATIC}"));
    assert_eq!(
        String::from_utf8_lossy(&o.stdout).lines().last(),
        Some(generation.to_str().unwrap()),
        "mirror prints the verified directory last: {}",
        text(&o)
    );
    assert_eq!(read(&generation.join("kernel-amd64")), FX_KERNEL);
    assert_eq!(read(&generation.join("initramfs-amd64.xz")), FX_INITRAMFS);

    // Verified bytes already here: the factory is not asked again.
    let o = run(&dir.join("no-factory"));
    assert_eq!(o.status.code(), Some(0), "{}", text(&o));

    // Bytes that changed on disk are not the declared ones: removed and
    // fetched again, never served as they stand.
    write_file(&generation.join("kernel-amd64"), "tampered\n");
    let o = run(&fac);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o));
    assert!(text(&o).contains("not the declared"), "{}", text(&o));
    assert_eq!(read(&generation.join("kernel-amd64")), FX_KERNEL);
}

#[test]
fn the_mirror_refuses_a_sha_mismatch_and_leaves_nothing_to_serve() {
    let dir = scratch_dir("netboot-mirror-mismatch");
    let wrong = sha256(b"some other initramfs\n");
    let decl = write_declaration(&dir, &sha256(FX_KERNEL.as_bytes()), &wrong, FX_ARGS);
    let fac = factory(&dir);
    let images = dir.join("images");
    let o = netboot(
        &["mirror", images.to_str().unwrap()],
        &[
            ("NETBOOT_DECLARATION", &decl),
            ("NETBOOT_FACTORY_URL", &file_url(&fac)),
        ],
    )
    .output()
    .unwrap();
    assert_eq!(o.status.code(), Some(1), "{}", text(&o));
    let out = text(&o);
    assert!(out.contains("REFUSED"), "{out}");
    assert!(out.contains(&wrong), "names the declared sha: {out}");
    assert!(
        out.contains(&sha256(FX_INITRAMFS.as_bytes())),
        "names the sha it got: {out}"
    );
    let generation = images.join(format!("{FX_VERSION}-{FX_SCHEMATIC}"));
    let left: Vec<String> = std::fs::read_dir(&generation)
        .map(|d| {
            d.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    assert!(
        !left.iter().any(|n| n.contains("initramfs")),
        "no initramfs, whole or partial, is left to serve: {left:?}"
    );
}

// ── what is rendered ─────────────────────────────────────────────────

#[test]
fn the_dnsmasq_config_is_proxy_dhcp_for_pxe_clients_only() {
    let url = "http://192.0.2.15:8079/boot.ipxe";
    let o = netboot(
        &["render-dnsmasq", FX_ADDR, "/var/lib/boss/netboot/tftp", url],
        &[],
    )
    .output()
    .unwrap();
    assert_eq!(o.status.code(), Some(0), "{}", text(&o));
    let conf = String::from_utf8_lossy(&o.stdout).into_owned();
    let lines: Vec<&str> = conf
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
        .collect();
    // Every directive is one this file knows: nothing that sets a lease,
    // a route, a resolver or a host reservation can ride in.
    let allowed = [
        "port",
        "listen-address",
        "dhcp-range",
        "leasefile-ro",
        "log-dhcp",
        "pid-file",
        "user",
        "group",
        "enable-tftp",
        "tftp-root",
        "dhcp-userclass",
        "pxe-service",
        // Who is answered: a tag per declared MAC, and everyone without
        // it ignored. Neither carries an address.
        "dhcp-mac",
        "dhcp-ignore",
    ];
    for l in &lines {
        let key = l.split('=').next().unwrap();
        assert!(allowed.contains(&key), "unexpected directive: {l}\n{conf}");
    }
    assert!(lines.contains(&"port=0"), "no DNS: {conf}");
    assert!(lines.contains(&"leasefile-ro"), "no lease database: {conf}");
    let ranges: Vec<&&str> = lines
        .iter()
        .filter(|l| l.starts_with("dhcp-range="))
        .collect();
    assert_eq!(
        ranges,
        vec![&format!("dhcp-range={FX_ADDR},proxy").as_str()],
        "one range, proxy mode: no address is ever leased\n{conf}"
    );
    assert!(lines.contains(&format!("listen-address={FX_ADDR}").as_str()));
    assert!(lines.contains(&"tftp-root=/var/lib/boss/netboot/tftp"));
    assert!(lines.contains(&"dhcp-userclass=set:ipxe,iPXE"));
    assert_eq!(
        lines
            .iter()
            .filter(|l| l.starts_with("dhcp-ignore="))
            .collect::<Vec<_>>(),
        vec![&"dhcp-ignore=tag:!netboot"],
        "every client without the netboot tag is ignored, whatever the declaration says\n{conf}"
    );
    // Firmware PXE is chained to iPXE over TFTP; iPXE is sent the script.
    for (csa, file) in [
        ("x86PC", "undionly.kpxe"),
        ("X86-64_EFI", "ipxe.efi"),
        ("BC_EFI", "ipxe.efi"),
    ] {
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with(&format!("pxe-service=tag:!ipxe,{csa},"))
                    && l.ends_with(&format!(",{file}"))),
            "{csa} firmware gets {file}: {conf}"
        );
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with(&format!("pxe-service=tag:ipxe,{csa},"))
                    && l.ends_with(&format!(",{url}"))),
            "{csa} iPXE gets the script: {conf}"
        );
    }
}

#[test]
fn the_ipxe_script_boots_the_declared_pair_with_the_factorys_args() {
    let base = "http://192.0.2.15:8079";
    let o = netboot(&["render-ipxe", base], &[]).output().unwrap();
    assert_eq!(o.status.code(), Some(0), "{}", text(&o));
    let body = String::from_utf8_lossy(&o.stdout).into_owned();
    let lines: Vec<&str> = body
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#') || l.starts_with("#!"))
        .collect();
    let at = format!(
        "{base}/talos/{}/{}",
        declared("version"),
        declared("schematic")
    );
    // The gate first: each declared MAC jumps to the boot, every other
    // NIC exits to the firmware's next boot device (its disk).
    let mut want: Vec<String> = vec!["#!ipxe".to_string()];
    want.extend(
        declared("netboot_macs")
            .split(' ')
            .filter(|m| !m.is_empty())
            .map(|m| format!("iseq ${{netX/mac}} {m} && goto talos ||")),
    );
    want.extend([
        "echo BOSS netboot: ${netX/mac} is not declared in infra/cluster/talos/netboot.toml - exiting to the next boot device".to_string(),
        "exit".to_string(),
        ":talos".to_string(),
    ]);
    assert_eq!(
        lines,
        [
            want,
            vec![
                "imgfree".to_string(),
                // The factory's args verbatim, plus the one iPXE needs to hand a
                // UEFI kernel its initramfs (the script says why).
                format!(
                    "kernel {at}/kernel-amd64 {} initrd=initramfs-amd64.xz",
                    declared("kernel_args")
                ),
                format!("initrd {at}/initramfs-amd64.xz"),
                "boot".to_string(),
            ]
        ]
        .concat(),
        "{body}"
    );
}

// ── the server ───────────────────────────────────────────────────────

struct Served {
    child: std::process::Child,
    port: u16,
}

impl Drop for Served {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn serve_env(dir: &Path, kernel_sha: &str) -> Command {
    let images = dir.join("images");
    create_dir(&images);
    write_file(&images.join("kernel-amd64"), FX_KERNEL);
    write_file(&images.join("initramfs-amd64.xz"), FX_INITRAMFS);
    write_file(&dir.join("boot.ipxe"), "#!ipxe\nboot\n");
    let mut c = Command::new("python3");
    c.arg("-I")
        .arg(repo_root().join("infra/forge/netboot-http.py"))
        .env_clear()
        .env("NETBOOT_BIND", "127.0.0.1")
        .env("NETBOOT_PORT", "0")
        .env("NETBOOT_IMAGES", &images)
        .env("NETBOOT_SCRIPT", dir.join("boot.ipxe"))
        .env("NETBOOT_PREFIX", "/talos/v0.0.1/fixture")
        .env("NETBOOT_KERNEL_SHA256", kernel_sha)
        .env("NETBOOT_INITRAMFS_SHA256", sha256(FX_INITRAMFS.as_bytes()))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    c
}

fn get(port: u16, path: &str) -> (String, Vec<u8>) {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    write!(s, "GET {path} HTTP/1.0\r\nHost: fixture\r\n\r\n").unwrap();
    let mut raw = Vec::new();
    s.read_to_end(&mut raw).unwrap();
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("a header block");
    let head = String::from_utf8_lossy(&raw[..split]).into_owned();
    (head, raw[split + 4..].to_vec())
}

#[test]
fn the_server_serves_its_three_names_and_nothing_else() {
    let dir = scratch_dir("netboot-http");
    let mut child = serve_env(&dir, &sha256(FX_KERNEL.as_bytes()))
        .spawn()
        .unwrap();
    let mut first = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut first)
        .unwrap();
    let port: u16 = first
        .trim()
        .rsplit(':')
        .next()
        .and_then(|p| p.parse().ok())
        .unwrap_or_else(|| panic!("the server names where it listens: {first:?}"));
    let served = Served { child, port };

    let (head, body) = get(served.port, "/boot.ipxe");
    assert!(head.starts_with("HTTP/1.0 200"), "{head}");
    assert_eq!(body, b"#!ipxe\nboot\n");
    let (head, body) = get(served.port, "/talos/v0.0.1/fixture/kernel-amd64");
    assert!(head.starts_with("HTTP/1.0 200"), "{head}");
    assert_eq!(body, FX_KERNEL.as_bytes());
    let (head, body) = get(served.port, "/talos/v0.0.1/fixture/initramfs-amd64.xz");
    assert!(head.starts_with("HTTP/1.0 200"), "{head}");
    assert_eq!(body, FX_INITRAMFS.as_bytes());
    for other in [
        "/",
        "/kernel-amd64",
        "/talos/v0.0.1/fixture/../../../boot.ipxe",
        "/boot.ipxe?x=1",
        "/etc/passwd",
    ] {
        let (head, _) = get(served.port, other);
        assert!(head.starts_with("HTTP/1.0 404"), "{other}: {head}");
    }
}

#[test]
fn the_server_refuses_to_start_on_bytes_it_cannot_verify() {
    let dir = scratch_dir("netboot-http-refuse");
    let wrong = sha256(b"not the kernel\n");
    let o = serve_env(&dir, &wrong).output().unwrap();
    assert_eq!(o.status.code(), Some(1), "{}", text(&o));
    let out = text(&o);
    assert!(out.contains("REFUSED"), "{out}");
    assert!(out.contains(&wrong), "{out}");
    assert!(!out.contains("serving"), "it never listened: {out}");
}

// ── ensure: the converge step ────────────────────────────────────────

struct Forge {
    dir: PathBuf,
    decl: PathBuf,
}

impl Forge {
    fn new(name: &str, decl: impl Fn(&Path) -> PathBuf) -> Self {
        let dir = scratch_dir(name);
        let decl = decl(&dir);
        factory(&dir);
        create_dir(&dir.join("etc"));
        create_dir(&dir.join("bin"));
        create_dir(&dir.join("ipxe"));
        write_file(&dir.join("ipxe/ipxe.efi"), "fixture ipxe.efi\n");
        write_file(&dir.join("ipxe/undionly.kpxe"), "fixture undionly.kpxe\n");
        write_exec(&dir.join("bin/dnsmasq"), "#!/bin/sh\nexit 0\n");
        write_exec(
            &dir.join("bin/systemctl"),
            &format!(
                "#!/bin/sh\necho \"$*\" >> '{}'\n[ \"$1\" = is-active ] && echo active\nexit 0\n",
                dir.join("systemctl.log").display()
            ),
        );
        write_exec(
            &dir.join("bin/apt-get"),
            &format!(
                "#!/bin/sh\necho \"$*\" >> '{}'\nexit 0\n",
                dir.join("apt-get.log").display()
            ),
        );
        write_file(
            &dir.join("sor.env"),
            &format!("BOSS_FORGE_HOST={FX_ADDR}\nBOSS_JOBS_URL=http://192.0.2.34:7900\n"),
        );
        Forge { dir, decl }
    }

    fn ensure(&self) -> Output {
        let d = &self.dir;
        netboot(
            &["ensure"],
            &[
                ("INSTALL_ETC", &d.join("etc")),
                ("INSTALL_SYSTEMCTL", &d.join("bin/systemctl")),
                ("INSTALL_APT_GET", &d.join("bin/apt-get")),
                ("INSTALL_NETBOOT_ROOT", &d.join("netboot")),
                ("NETBOOT_IPXE_DIR", &d.join("ipxe")),
                ("NETBOOT_DNSMASQ", &d.join("bin/dnsmasq")),
                ("NETBOOT_DECLARATION", &self.decl),
                ("NETBOOT_FACTORY_URL", &file_url(&d.join("factory"))),
                ("BOSS_SOR_ENV", &d.join("sor.env")),
                ("BOSS_RUN_SUMMARY_FILE", &d.join("summary.json")),
            ],
        )
        .output()
        .unwrap()
    }

    fn systemctl(&self) -> String {
        std::fs::read_to_string(self.dir.join("systemctl.log")).unwrap_or_default()
    }
}

#[test]
fn ensure_converges_a_scratch_forge_and_restarts_only_on_change() {
    let f = Forge::new("netboot-ensure", good_declaration);
    let o = f.ensure();
    assert_eq!(o.status.code(), Some(0), "{}", text(&o));
    let nb = f.dir.join("netboot");
    for unit in ["forge-netboot-dhcp.service", "forge-netboot-http.service"] {
        assert_eq!(
            read(&f.dir.join("etc").join(unit)),
            read(&repo_root().join("infra/forge").join(unit)),
            "{unit} is installed from the tree as it stands"
        );
    }
    assert_eq!(read(&nb.join("tftp/ipxe.efi")), "fixture ipxe.efi\n");
    assert_eq!(
        read(&nb.join("tftp/undionly.kpxe")),
        "fixture undionly.kpxe\n"
    );
    let conf = read(&nb.join("dnsmasq.conf"));
    assert!(
        conf.contains(&format!("dhcp-range={FX_ADDR},proxy")),
        "{conf}"
    );
    assert!(
        conf.contains(&format!("tftp-root={}", nb.join("tftp").display())),
        "{conf}"
    );
    let ipxe = read(&nb.join("boot.ipxe"));
    assert!(
        ipxe.contains(&format!(
            "kernel http://{FX_ADDR}:8079/talos/{FX_VERSION}/{FX_SCHEMATIC}/kernel-amd64 {FX_ARGS} "
        )),
        "{ipxe}"
    );
    let env = read(&nb.join("serve.env"));
    for want in [
        format!("NETBOOT_BIND={FX_ADDR}"),
        "NETBOOT_PORT=8079".to_string(),
        format!("NETBOOT_KERNEL_SHA256={}", sha256(FX_KERNEL.as_bytes())),
        format!(
            "NETBOOT_INITRAMFS_SHA256={}",
            sha256(FX_INITRAMFS.as_bytes())
        ),
        format!(
            "NETBOOT_IMAGES={}",
            nb.join(format!("images/{FX_VERSION}-{FX_SCHEMATIC}"))
                .display()
        ),
    ] {
        assert!(env.lines().any(|l| l == want), "{want} in serve.env: {env}");
    }
    assert_eq!(
        read(&nb.join("netboot-http.py")),
        read(&repo_root().join("infra/forge/netboot-http.py"))
    );
    let log = f.systemctl();
    assert!(log.contains("daemon-reload"), "{log}");
    assert!(log.contains("restart forge-netboot-dhcp.service"), "{log}");
    assert!(log.contains("restart forge-netboot-http.service"), "{log}");
    // The fixture declares no MAC: the forge converges, and answers nobody.
    assert!(conf.contains("\ndhcp-ignore=tag:!netboot\n"), "{conf}");
    assert!(!conf.contains("dhcp-mac="), "{conf}");
    let summary = read(&f.dir.join("summary.json"));
    assert!(summary.contains("netboot"), "{summary}");
    assert!(
        summary.contains("answering 0 declared MAC(s)"),
        "the converge says who it answers: {summary}"
    );

    // A second tick with nothing changed starts what is stopped and
    // restarts nothing.
    std::fs::remove_file(f.dir.join("systemctl.log")).unwrap();
    let o = f.ensure();
    assert_eq!(o.status.code(), Some(0), "{}", text(&o));
    let log = f.systemctl();
    assert!(!log.contains("restart"), "{log}");
    assert!(
        log.contains("enable --now forge-netboot-dhcp.service"),
        "{log}"
    );
    assert!(
        log.contains("enable --now forge-netboot-http.service"),
        "{log}"
    );
}

#[test]
fn ensure_serves_nothing_when_the_declared_bytes_cannot_be_had() {
    let f = Forge::new("netboot-ensure-mismatch", |d| {
        write_declaration(
            d,
            &sha256(FX_KERNEL.as_bytes()),
            &sha256(b"not what the factory has\n"),
            FX_ARGS,
        )
    });
    let o = f.ensure();
    assert_eq!(o.status.code(), Some(1), "{}", text(&o));
    assert!(text(&o).contains("REFUSED"), "{}", text(&o));
    let log = f.systemctl();
    assert!(log.contains("stop forge-netboot-http.service"), "{log}");
    assert!(log.contains("stop forge-netboot-dhcp.service"), "{log}");
    assert!(!log.contains("restart"), "{log}");
    assert!(!log.contains("enable --now"), "{log}");
    assert!(!f.dir.join("netboot/serve.env").exists());
}

#[test]
fn ensure_refuses_with_one_seam_forgotten() {
    for seam in [
        "INSTALL_ETC",
        "INSTALL_SYSTEMCTL",
        "INSTALL_APT_GET",
        "INSTALL_NETBOOT_ROOT",
    ] {
        let f = Forge::new("netboot-ensure-seam", good_declaration);
        let d = &f.dir;
        let mut c = netboot(
            &["ensure"],
            &[
                ("INSTALL_ETC", &d.join("etc")),
                ("INSTALL_SYSTEMCTL", &d.join("bin/systemctl")),
                ("INSTALL_APT_GET", &d.join("bin/apt-get")),
                ("INSTALL_NETBOOT_ROOT", &d.join("netboot")),
                ("BOSS_SOR_ENV", &d.join("sor.env")),
            ],
        );
        c.env_remove(seam);
        let o = c.output().unwrap();
        assert_eq!(o.status.code(), Some(78), "{seam} forgotten: {}", text(&o));
        assert!(text(&o).contains(seam), "{}", text(&o));
        assert!(!d.join("netboot").exists(), "{seam}: nothing written");
        assert!(!d.join("systemctl.log").exists(), "{seam}: nothing asked");
    }
}

#[test]
fn the_forge_converge_runs_ensure_and_carries_its_failure() {
    let install = read(&repo_root().join("infra/forge/install.sh"));
    assert!(
        install.contains("bash \"${HERE}/netboot.sh\" ensure || netboot_rc=$?"),
        "install.sh runs netboot.sh ensure, carried"
    );
    assert!(
        install.contains("exit \"$netboot_rc\""),
        "a failed netboot reds the converge"
    );
}
