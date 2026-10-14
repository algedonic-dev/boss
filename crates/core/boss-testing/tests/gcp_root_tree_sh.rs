//! The dormant GCP tree fetches protected main over authenticated HTTP,
//! preserves native Git history, and never replaces a good tree on refusal.
use boss_testing::{copy_exec, create_dir, repo_root, scratch_dir, write_exec, write_file};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

fn git(dir: &Path, args: &[&str]) -> String {
    let o = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=fixture",
            "-c",
            "user.email=fixture@example.invalid",
        ])
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(o.status.success(), "{args:?}: {}", text(&o));
    String::from_utf8_lossy(&o.stdout).trim().to_owned()
}

fn published_port(
    mut read: impl FnMut() -> std::io::Result<String>,
    mut child: impl FnMut() -> std::io::Result<Option<std::process::ExitStatus>>,
    mut expired: impl FnMut() -> bool,
    mut pause: impl FnMut(),
) -> Result<u16, String> {
    loop {
        match child() {
            Ok(None) => {}
            Ok(Some(status)) => return Err(format!("server exited before readiness: {status}")),
            Err(error) => return Err(format!("cannot observe server liveness: {error}")),
        }
        let observation = read();
        if expired() {
            return Err(format!(
                "port publication deadline expired; last read: {observation:?}"
            ));
        }
        if let Ok(bytes) = &observation
            && let Ok(port) = bytes.trim().parse::<u16>()
            && port != 0
        {
            return Ok(port);
        }
        pause();
    }
}

#[test]
fn readiness_waits_through_an_existing_empty_port_file() {
    let dir = scratch_dir("gcp-root-tree-port-publication");
    let path = dir.join("port");
    write_file(&path, "");
    let mut reads = 0;
    let port = published_port(
        || {
            let observed = std::fs::read_to_string(&path);
            reads += 1;
            if reads == 1 {
                // Publish after the reader has observed the actual empty file.
                write_file(&path, "35809");
            }
            observed
        },
        || Ok(None),
        || false,
        || {},
    )
    .unwrap();
    assert_eq!(port, 35809);
    assert_eq!(reads, 2);
}

#[test]
fn readiness_rejects_invalid_publications_until_a_valid_port() {
    let mut observations = ["", "0", "65536", "not a port", "35809\n"].into_iter();
    assert_eq!(
        published_port(
            || Ok(observations.next().unwrap().to_owned()),
            || Ok(None),
            || false,
            || {}
        )
        .unwrap(),
        35809
    );
    assert!(observations.next().is_none());
}

#[test]
fn readiness_refuses_dead_child_and_retains_deadline_read_error() {
    use std::os::unix::process::ExitStatusExt;
    let dead = published_port(
        || panic!("dead server must not read readiness"),
        || Ok(Some(std::process::ExitStatus::from_raw(7 << 8))),
        || false,
        || {},
    );
    assert!(dead.unwrap_err().contains("exit status: 7"));
    let unavailable = published_port(
        || panic!("unknown server must not read readiness"),
        || Err(std::io::Error::other("liveness unavailable")),
        || false,
        || {},
    );
    assert!(unavailable.unwrap_err().contains("liveness unavailable"));
    let deadline = published_port(
        || Err(std::io::Error::other("port publication unavailable")),
        || Ok(None),
        || true,
        || panic!("expired deadline must not sleep"),
    );
    let diagnostic = deadline.unwrap_err();
    assert!(diagnostic.contains("deadline expired"));
    assert!(diagnostic.contains("port publication unavailable"));
}

struct Server(Child);
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn server_port(
    mut server: Server,
    dir: &Path,
    timeout: std::time::Duration,
) -> Result<(Server, u16), String> {
    let port_path = dir.join("port");
    let started = std::time::Instant::now();
    let result = published_port(
        || std::fs::read_to_string(&port_path),
        || server.0.try_wait(),
        || started.elapsed() >= timeout,
        || std::thread::sleep(std::time::Duration::from_millis(10)),
    );
    match result {
        Ok(port) => Ok((server, port)),
        Err(error) => {
            // Reap before reading stderr: a failed constructor must neither
            // leak its server nor lose the writer's final diagnostic bytes.
            let stopped = server.0.kill();
            let reaped = server.0.wait();
            Err(format!(
                "HTTP fixture did not start: {error}; elapsed={:?}; timeout={timeout:?}; port={}; kill={stopped:?}; wait={reaped:?}; stderr={:?}",
                started.elapsed(),
                port_path.display(),
                std::fs::read_to_string(dir.join("server.stderr"))
            ))
        }
    }
}

#[test]
fn startup_failure_preserves_complete_native_stderr() {
    let dir = scratch_dir("gcp-root-tree-startup-failure");
    let stderr = dir.join("server.stderr");
    let mut server = Server(
        Command::new("sh")
            .args([
                "-c",
                "printf 'first diagnostic\\nlast diagnostic\\n' >&2; exit 7",
            ])
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(&stderr).unwrap())
            .spawn()
            .unwrap(),
    );
    assert_eq!(server.0.wait().unwrap().code(), Some(7));
    let error = match server_port(server, &dir, std::time::Duration::from_secs(1)) {
        Ok(_) => panic!("an exited server cannot become ready"),
        Err(error) => error,
    };
    assert!(error.contains("exit status: 7"), "{error}");
    assert!(
        error.contains("first diagnostic\\nlast diagnostic\\n"),
        "{error}"
    );
    assert!(
        error.contains(stderr.parent().unwrap().to_str().unwrap()),
        "{error}"
    );
}

#[test]
fn startup_deadline_reaps_the_live_child_with_empty_publication() {
    let dir = scratch_dir("gcp-root-tree-startup-deadline");
    write_file(&dir.join("port"), "");
    let server = Server(
        Command::new("sleep")
            .arg("30")
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(dir.join("server.stderr")).unwrap())
            .spawn()
            .unwrap(),
    );
    let error = match server_port(server, &dir, std::time::Duration::ZERO) {
        Ok(_) => panic!("an empty publication cannot become ready"),
        Err(error) => error,
    };
    assert!(error.contains("deadline expired"), "{error}");
    assert!(error.contains("last read: Ok(\"\")"), "{error}");
    assert!(error.contains("kill=Ok(())"), "{error}");
    assert!(error.contains("wait=Ok("), "{error}");
}

struct Host {
    dir: PathBuf,
    source: PathBuf,
    tree: PathBuf,
    url: String,
    _server: Server,
}
impl Host {
    fn new(name: &str) -> Self {
        let dir = scratch_dir(&format!("gcp-root-tree-{name}"));
        let source = dir.join("source");
        create_dir(&source);
        git(&source, &["init", "-q", "-b", "main"]);
        for path in [
            "infra/gcp/root-tree.sh",
            "infra/gcp/boss-gcp-converge.sh",
            "infra/gcp/adopt-root-tree.sh",
            "infra/gcp/prepare-root-tree.sh",
            "infra/gcp/install-units.sh",
            "infra/gcp/root-tree-status.sh",
            "infra/ops/ops-runner.sh",
            "infra/codebase-metrics.sh",
            "infra/protocol-drift.sh",
            "infra/surface-usage.sh",
            "infra/ml/run-inference-batch.sh",
            "infra/boss-maintenance-wrap.sh",
            "infra/boss-step.sh",
        ] {
            create_dir(source.join(path).parent().unwrap());
            write_exec(&source.join(path), "#!/bin/sh\nexit 0\n");
        }
        git(&source, &["add", "-A"]);
        git(&source, &["commit", "-q", "-m", "first"]);
        create_dir(&dir.join("http/david"));
        git(
            &source,
            &[
                "clone",
                "-q",
                "--bare",
                ".",
                dir.join("http/david/boss.git").to_str().unwrap(),
            ],
        );
        let credential = dir.join("credential");
        create_dir(&credential);
        // mode-bits-ok: synthetic private credential storage, never executable.
        std::fs::set_permissions(&credential, std::fs::Permissions::from_mode(0o700)).unwrap();
        write_file(&credential.join("token"), "fixture-token\n");
        std::fs::set_permissions(
            credential.join("token"),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        let server_file = dir.join("server.py");
        write_file(
            &server_file,
            r#"import http.server, os, pathlib, subprocess, sys
root=pathlib.Path(sys.argv[1])
class Handler(http.server.BaseHTTPRequestHandler):
 def log_message(self,*args): pass
 def do_GET(self): self.serve()
 def do_POST(self): self.serve()
 def serve(self):
  if self.path.startswith('/api/jobs?'):
   import json, urllib.parse
   query=urllib.parse.parse_qs(urllib.parse.urlparse(self.path).query)
   job=dict(kind=query['kind'][0],subject=dict(id=query['subject_id'][0]),opened_at='2026-10-13T00:00:00Z',status='closed',steps=[dict(spec_slug=s,status='completed',completed_by='emp-fixture') for s in ['scope','verify','revoke']])
   self.send_response(200); self.end_headers(); self.wfile.write(json.dumps(dict(total=1,data=[job])).encode()); return
  if self.path == '/api/v1/repos/david/boss':
   if self.headers.get('Authorization') != 'token fixture-token': self.send_response(401); self.end_headers(); return
   self.send_response(200); self.end_headers(); self.wfile.write(b'{"full_name":"david/boss"}'); return
  if self.headers.get('Authorization') != 'Basic ZGF2aWQ6Zml4dHVyZS10b2tlbg==':
   self.send_response(401); self.send_header('WWW-Authenticate','Basic realm="fixture"'); self.end_headers(); return
  with (root/'authenticated').open('a') as f: f.write('request\n')
  path,_,query=self.path.partition('?')
  env=dict(os.environ,GIT_PROJECT_ROOT=str(root/'http'),GIT_HTTP_EXPORT_ALL='1',PATH_INFO=path,QUERY_STRING=query,REQUEST_METHOD=self.command,CONTENT_TYPE=self.headers.get('Content-Type',''),REMOTE_USER='david')
  body=self.rfile.read(int(self.headers.get('Content-Length','0')))
  out=subprocess.run(['git','http-backend'],input=body,capture_output=True,env=env)
  if out.returncode: self.send_response(500); self.end_headers(); self.wfile.write(out.stderr); return
  headers,body=out.stdout.split(b'\r\n\r\n',1)
  lines=[line.decode().split(': ',1) for line in headers.split(b'\r\n')]
  status=next((int(v.split()[0]) for k,v in lines if k.lower()=='status'),200)
  self.send_response(status)
  for k,v in lines:
   if k.lower()!='status': self.send_header(k,v)
  self.end_headers(); self.wfile.write(body)
server=http.server.HTTPServer(('127.0.0.1',0),Handler)
(root/'port').write_text(str(server.server_port))
server.serve_forever()
"#,
        );
        let server = Server(
            Command::new("python3")
                .arg(&server_file)
                .arg(&dir)
                .stdout(Stdio::null())
                .stderr(std::fs::File::create(dir.join("server.stderr")).unwrap())
                .spawn()
                .unwrap(),
        );
        let (server, port) = server_port(server, &dir, std::time::Duration::from_secs(1))
            .unwrap_or_else(|error| panic!("{error}"));
        let tree = dir.join("tree");
        Self {
            dir,
            source,
            tree,
            url: format!("http://127.0.0.1:{port}"),
            _server: server,
        }
    }
    fn run(&self, mode: &str) -> Output {
        self.run_args(&[mode])
    }
    fn run_args(&self, args: &[&str]) -> Output {
        Command::new("bash")
            .arg(repo_root().join("infra/gcp/root-tree.sh"))
            .args(args)
            .env("BOSS_GCP_ROOT_TREE", &self.tree)
            .env("BOSS_FORGE_URL", &self.url)
            .env(
                "BOSS_GCP_ROOT_TREE_TOKEN",
                self.dir.join("credential/token"),
            )
            .env("BOSS_SOR_ENV", self.dir.join("absent.env"))
            .env("GIT_CONFIG_COUNT", "1")
            .env("GIT_CONFIG_KEY_0", "url.file:///untrusted/.insteadOf")
            .env("GIT_CONFIG_VALUE_0", &self.url)
            .env("GIT_DIR", self.dir.join("foreign.git"))
            .output()
            .unwrap()
    }
    fn refresh(&self) {
        let o = self.run("refresh");
        assert!(o.status.success(), "{}", text(&o));
    }
    fn path(&self) -> PathBuf {
        let o = self.run("path");
        assert!(o.status.success(), "{}", text(&o));
        PathBuf::from(String::from_utf8_lossy(&o.stdout).trim())
    }
    fn publish(&self) {
        git(
            &self.source,
            &[
                "push",
                "-q",
                "--force",
                self.dir.join("http/david/boss.git").to_str().unwrap(),
                "main",
            ],
        );
    }
}

#[test]
fn a_generation_without_the_converge_entrypoint_cannot_replace_current() {
    let h = Host::new("missing-converge");
    h.refresh();
    let accepted = h.path();
    std::fs::remove_file(h.source.join("infra/gcp/boss-gcp-converge.sh")).unwrap();
    git(&h.source, &["add", "-A"]);
    git(&h.source, &["commit", "-qm", "missing converge"]);
    h.publish();
    let out = h.run("refresh");
    assert_eq!(out.status.code(), Some(78), "{}", text(&out));
    assert_eq!(h.path(), accepted);
}

// not a tree-wide pin: the authenticated fixture carries only this handover's
// GCP, library and estate closure, never the workspace or unrelated crates.
fn copy_adoption_dir(from: &Path, to: &Path) {
    create_dir(to);
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_dir() {
            copy_adoption_dir(&entry.path(), &to.join(entry.file_name()));
        } else if entry.file_type().unwrap().is_file() {
            copy_exec(&entry.path(), &to.join(entry.file_name()));
        }
    }
}

#[test]
fn adoption_fetches_authenticated_source_then_launcher_accepts_the_same_generation() {
    let h = Host::new("adoption");
    for name in ["gcp", "lib", "estate", "ops", "ml"] {
        copy_adoption_dir(
            &repo_root().join("infra").join(name),
            &h.source.join("infra").join(name),
        );
    }
    for name in [
        "run-summary.sh",
        "boss-codebase-metrics.service",
        "boss-codebase-metrics.timer",
        "boss-protocol-drift.service",
        "boss-protocol-drift.timer",
        "boss-surface-usage.service",
        "boss-surface-usage.timer",
    ] {
        std::fs::copy(
            repo_root().join("infra").join(name),
            h.source.join("infra").join(name),
        )
        .unwrap();
    }
    create_dir(&h.source.join("infra/forge"));
    std::fs::copy(
        repo_root().join("infra/forge/sor-ports.env"),
        h.source.join("infra/forge/sor-ports.env"),
    )
    .unwrap();
    // Fixed boundary adapters: no native credential installer, network registry,
    // journal write or image install runs in this authenticated source fixture.
    write_exec(
        &h.source.join("infra/estate/node-roles.sh"),
        "has_role() { [ \"$1\" = ops-runner ]; }\nread_node_roles() { export BOSS_NODE_ROLES=ops-runner BOSS_NODE_ROLES_SOURCE=registry; }\n",
    );
    write_exec(
        &h.source.join("infra/estate/install-cluster-operator.sh"),
        "install_cluster_operator() { exit 99; }\n",
    );
    for name in [
        "estate/machine-token-effect.sh",
        "gcp/cap-journal.sh",
        "journal-door-ensure.sh",
    ] {
        write_exec(&h.source.join("infra").join(name), "#!/bin/sh\nexit 0\n");
    }
    for name in [
        "install-runner-credential-receiver.sh",
        "install-root-tree-token-receiver.sh",
    ] {
        write_exec(
            &h.source.join("infra/gcp").join(name),
            "#!/bin/sh\nprintf '%s\\n' \"$INSTALL_RECV_USER\" >> \"$RECEIVER_LOG\"\n",
        );
    }
    git(&h.source, &["add", "-A"]);
    git(&h.source, &["commit", "-qm", "real adoption closure"]);
    h.publish();
    let prepared = h.dir.join("prepared");
    let out = Command::new("bash")
        .arg(repo_root().join("infra/gcp/adopt-root-tree.sh"))
        .args(["bootstrap", "fixture"])
        .env("BOSS_GCP_PREPARED_DIR", &prepared)
        .env("BOSS_GCP_ROOT_TREE", &h.tree)
        .env("BOSS_FORGE_URL", &h.url)
        .env("BOSS_JOBS_URL", &h.url)
        .env(
            "BOSS_GCP_TRUSTED_CREDENTIAL_HELPER",
            h.source.join("infra/gcp/root-tree-credential.sh"),
        )
        .env("BOSS_GCP_ROOT_TREE_TOKEN", h.dir.join("credential/token"))
        .env("BOSS_MACHINE_TOKEN_DIR", h.dir.join("absent-machine-token"))
        .env("BOSS_SOR_ENV", h.dir.join("absent.env"))
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", text(&out));
    assert!(
        !h.tree.exists(),
        "bootstrap does not adopt current or install units"
    );
    assert_eq!(
        std::fs::read_to_string(prepared.join("bootstrap/deposit-user")).unwrap(),
        "fixture\n"
    );
    assert_eq!(
        std::fs::read(prepared.join("bootstrap/infra/gcp/root-tree.sh")).unwrap(),
        std::fs::read(h.source.join("infra/gcp/root-tree.sh")).unwrap()
    );
    h.refresh();
    let accepted = h.path();
    assert_eq!(
        accepted.file_name().unwrap(),
        git(&h.source, &["rev-parse", "HEAD"]).as_str()
    );
    let units = Command::new("bash")
        .arg(prepared.join("bootstrap/infra/gcp/prepare-root-tree.sh"))
        .arg("units")
        .arg(accepted.file_name().unwrap())
        .env("BOSS_GCP_PREPARED_DIR", &prepared)
        .env("BOSS_GCP_ROOT_TREE", &h.tree)
        .output()
        .unwrap();
    assert!(units.status.success(), "{}", text(&units));
    assert_eq!(
        std::fs::read_dir(prepared.join("units")).unwrap().count(),
        5
    );
    let systemctl = h.dir.join("status-systemctl");
    write_exec(
        &systemctl,
        r#"#!/bin/bash
case "$2" in boss-gcp-converge.service|boss-ops-runner.service) executable="$BOSS_GCP_PREPARED_DIR/bootstrap/infra/gcp/gcp-root-launch.sh";; *) executable="$BOSS_GCP_ROOT_TREE/current/infra/gcp/root-tree.sh";; esac
user=root
if [ "${STATUS_UNADOPTED:-}" = 1 ] && [ "$2" = boss-ml-inference-batch.service ]; then user=boss; fi
printf 'User=%s\nLoadState=loaded\nActiveState=inactive\nExecStart={ path=%s ; argv[]=%s secret-sentinel ; }\n' "$user" "$executable" "$executable"
"#,
    );
    let status = |unadopted: bool| {
        Command::new("bash")
            .arg(repo_root().join("infra/gcp/root-tree-status.sh"))
            .env("BOSS_GCP_PREPARED_DIR", &prepared)
            .env("BOSS_GCP_ROOT_TREE", &h.tree)
            .env("BOSS_GCP_STATUS_SYSTEMCTL", &systemctl)
            .env("STATUS_UNADOPTED", if unadopted { "1" } else { "0" })
            .output()
            .unwrap()
    };
    let proven = status(false);
    assert!(proven.status.success(), "{}", text(&proven));
    assert_eq!(text(&proven).matches("verdict=adopted").count(), 6);
    assert!(!text(&proven).contains("secret-sentinel"));
    let held = status(true);
    assert_eq!(held.status.code(), Some(1), "{}", text(&held));
    assert!(text(&held).contains("unit=boss-ml-inference-batch user=boss"));
    assert!(!text(&held).contains("secret-sentinel"));
    let etc = h.dir.join("etc");
    create_dir(&etc);
    let native_systemctl = h.dir.join("install-systemctl");
    write_exec(&native_systemctl, "#!/bin/sh\nexit 0\n");
    let cli = h.dir.join("image-installer");
    write_exec(&cli, "#!/bin/sh\nexit 0\n");
    let summary = h.dir.join("summary.json");
    let convergence = Command::new("bash")
        .arg(prepared.join("bootstrap/infra/gcp/gcp-root-launch.sh"))
        .arg("converge")
        .env("BOSS_GCP_PREPARED_DIR", &prepared)
        .env("BOSS_GCP_ROOT_TREE", &h.tree)
        .env("BOSS_FORGE_URL", &h.url)
        .env("BOSS_JOBS_URL", &h.url)
        .env("BOSS_GCP_ROOT_TREE_TOKEN", h.dir.join("credential/token"))
        .env("BOSS_GCP_REPO_DIR", &h.source)
        .env("BOSS_GCP_CONVERGE_SOR_ENV", h.dir.join("sor.env"))
        .env("BOSS_JOURNALD_CONF_DIR", h.dir.join("journal"))
        .env("BOSS_JOURNALD_SYSTEMCTL", &native_systemctl)
        .env("INSTALL_TALOSCTL", "0")
        .env("BOSS_GCP_CONVERGE_CLI_INSTALLER", &cli)
        .env("INSTALL_ETC", &etc)
        .env("INSTALL_SYSTEMCTL", &native_systemctl)
        .env("INSTALL_APT_GET", h.dir.join("apt-get"))
        .env("BOSS_OPS_RUNNER_RETIRED", h.dir.join("retired"))
        .env(
            "BOSS_GCP_CONVERGE_AUTHORIZED_KEYS",
            h.dir.join("absent-keys"),
        )
        .env("BOSS_RUN_SUMMARY_FILE", &summary)
        .env("RECEIVER_LOG", h.dir.join("receiver-log"))
        .output()
        .unwrap();
    assert!(convergence.status.success(), "{}", text(&convergence));
    assert_eq!(
        std::fs::read_to_string(h.dir.join("receiver-log")).unwrap(),
        "fixture\nfixture\n"
    );
    let facts: serde_json::Value =
        serde_json::from_slice(&std::fs::read(summary).unwrap()).unwrap();
    assert_eq!(
        facts["converge_sha"],
        accepted.file_name().unwrap().to_str().unwrap()
    );
    assert_eq!(facts["converge_remote"], "protected-root");
    let runner =
        std::fs::read_to_string(etc.join("boss-ops-runner.service.d/boss-gcp.conf")).unwrap();
    assert!(runner.contains("/bootstrap/infra/gcp/gcp-root-launch.sh run infra/ops/ops-runner.sh"));
    assert!(runner.contains("Environment=BOSS_OPS_CHECKOUT=/opt/boss"));
}

#[test]
fn authenticated_network_main_preserves_history_and_old_generation_on_failure() {
    let h = Host::new("history");
    // Exercise the owning reader, rather than assuming that a directory
    // carrying an exported tree is enough for native metrics.
    for path in [
        "infra/codebase-metrics.sh",
        "infra/lint/lib/git-answer.sh",
        "infra/lint/lib/trunk-ref.sh",
        "infra/lib/secret-header.sh",
    ] {
        create_dir(h.source.join(path).parent().unwrap());
        write_exec(
            &h.source.join(path),
            &std::fs::read_to_string(repo_root().join(path)).unwrap(),
        );
    }
    git(&h.source, &["add", "-A"]);
    git(&h.source, &["commit", "-q", "-m", "native reader"]);
    h.publish();
    h.refresh();
    let one = h.path();
    let first = git(&h.source, &["rev-parse", "HEAD"]);
    assert_eq!(git(&one, &["rev-parse", "HEAD"]), first);
    assert_eq!(
        git(&one, &["log", "--first-parent", "--format=%s"]),
        "native reader\nfirst"
    );
    let native = Command::new("bash")
        .arg(one.join("infra/codebase-metrics.sh"))
        .args(["series", "--repo"])
        .arg(&one)
        .args(["--ref", "HEAD"])
        .output()
        .unwrap();
    assert!(native.status.success(), "{}", text(&native));
    let row: serde_json::Value = serde_json::from_slice(&native.stdout).unwrap();
    assert_eq!(row["head"], first);
    assert_eq!(row["window"]["landings"], 2);
    let routed = h.run_args(&[
        "run",
        "infra/codebase-metrics.sh",
        "series",
        "--repo",
        ".",
        "--ref",
        "HEAD",
    ]);
    assert!(
        routed.status.success(),
        "routed owning reader must not inherit foreign Git selection: {}",
        text(&routed)
    );
    let row: serde_json::Value = serde_json::from_slice(&routed.stdout).unwrap();
    assert_eq!(row["head"], first);
    assert!(
        h.dir.join("authenticated").exists(),
        "fetch must use real credential helper and HTTP challenge"
    );
    write_file(&h.source.join("landed"), "second\n");
    git(&h.source, &["add", "-A"]);
    git(&h.source, &["commit", "-q", "-m", "second"]);
    h.publish();
    h.refresh();
    let two = h.path();
    assert_ne!(one, two);
    assert_eq!(
        git(&two, &["log", "--first-parent", "--format=%s"]),
        "second\nnative reader\nfirst"
    );
    assert_eq!(
        git(&one, &["rev-parse", "HEAD"]),
        first,
        "old generation identity cannot move under reader"
    );
    git(&h.source, &["update-ref", "refs/heads/main", &first]);
    h.publish();
    let refused = h.run("refresh");
    assert_eq!(refused.status.code(), Some(3), "{}", text(&refused));
    assert_eq!(h.path(), two);
    write_file(&h.dir.join("credential/token"), "invalid-token\n");
    let refused = h.run("refresh");
    assert!(
        !refused.status.success(),
        "no anonymous or checkout fallback"
    );
    assert_eq!(h.path(), two);
    // mode-bits-ok: make the fixture directory writable to test refusal; no executable file is written.
    std::fs::set_permissions(&h.tree, std::fs::Permissions::from_mode(0o777)).unwrap();
    let refused = h.run("path");
    assert!(
        !refused.status.success(),
        "writable root tree is not trusted"
    );
}

#[test]
fn a_generation_marker_symlink_cannot_write_outside_the_staging_tree() {
    let h = Host::new("marker");
    let target = h.dir.join("unrelated");
    write_file(&target, "preserved\n");
    std::os::unix::fs::symlink(&target, h.source.join(".boss-tree-sha")).unwrap();
    git(&h.source, &["add", "-A"]);
    git(&h.source, &["commit", "-q", "-m", "malformed marker"]);
    h.publish();
    let o = h.run("refresh");
    assert!(
        !o.status.success(),
        "a tracked marker is not helper-owned metadata: {}",
        text(&o)
    );
    assert_eq!(std::fs::read_to_string(target).unwrap(), "preserved\n");
    assert!(!h.tree.join("current").exists());
}

#[test]
fn staged_units_are_root_only_and_are_not_current_installable_units() {
    let root = repo_root();
    for stem in [
        "boss-codebase-metrics",
        "boss-protocol-drift",
        "boss-surface-usage",
        "boss-ml-inference-batch",
    ] {
        let p = root
            .join("infra/gcp/root-tree-units")
            .join(format!("{stem}.service.in"));
        let source = std::fs::read_to_string(&p)
            .unwrap_or_else(|e| panic!("missing dormant template {}: {e}", p.display()));
        assert!(source.lines().any(|s| s == "User=root"));
        assert!(source.contains("WorkingDirectory=/var/lib/boss/gcp-tree/current"));
        assert!(source.contains("EnvironmentFile=/etc/boss/sor.env"));
        assert!(!source.contains("/opt/boss/"));
        assert!(
            source.contains("ExecStart=/var/lib/boss/gcp-tree/current/infra/gcp/root-tree.sh run "),
            "staged entrypoint must already exist in the trusted generation, not an imagined installed launcher"
        );
        assert!(!p.with_extension("timer").exists());
    }
    let rows = Command::new("bash")
        .arg(root.join("infra/gcp/install-units.sh"))
        .arg("rows")
        .env("BOSS_REPO_ROOT", &root)
        .output()
        .unwrap();
    assert!(rows.status.success(), "{}", text(&rows));
    assert!(
        !text(&rows).contains("root-tree-units"),
        "dormant templates must not be adopted by current converge"
    );
}

#[test]
fn a_missing_unit_entrypoint_cannot_replace_the_accepted_generation() {
    let h = Host::new("entrypoint-closure");
    h.refresh();
    let accepted = h.path();
    assert!(accepted.join("infra/gcp/root-tree.sh").is_file());
    git(&h.source, &["rm", "infra/gcp/root-tree.sh"]);
    git(&h.source, &["commit", "-q", "-m", "remove unit entrypoint"]);
    h.publish();
    let result = h.run("refresh");
    let current = h.path();
    assert!(
        !result.status.success(),
        "missing common unit entrypoint must be refused; current changed={}, entrypoint exists={}: {}",
        current != accepted,
        current.join("infra/gcp/root-tree.sh").is_file(),
        text(&result)
    );
    assert_eq!(
        current, accepted,
        "the last accepted generation must survive"
    );
}
