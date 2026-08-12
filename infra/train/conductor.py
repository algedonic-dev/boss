#!/usr/bin/env python3
"""conductor.py — drive the pr-train Workflow.

The train is the cadence: changes accumulate on branches with their
ship-a-change Jobs parked at `review`, and twice a day this runs and
does the batching a person used to do by discipline. Two phases:

 1. RECONCILE — for every OPEN pr-train Job, record whatever evidence
    arrived since the last run: the CI verdict (polled from gh), the
    merge (observed, never assumed), and the deploys that carried the
    merge out. Steps close only when the conductor holds the evidence
    in hand; a train whose PR nobody merged just stays open, visibly.

 2. BOARD — open this window's train Job, collect the ship-a-change
    Jobs that are ready (review step ready/active, a branch pushed to
    the fork, not already on a train), assemble one train branch by
    merging each on top of origin/main, push it, open ONE batched PR.
    A branch that does not merge cleanly is skipped, named on the Job,
    and left for the next train. An empty window cancels the train via
    the `job.metadata.empty` marker rather than pretending.

Two trees, deliberately:
  - assembly happens in a dedicated clone (BOSS_TRAIN_HOME/repo) —
    never in the dev working tree, which may hold a session's
    half-built work;
  - deploys run from the dev tree (/opt/boss) only when it is clean
    and on main; otherwise the deploy is left pending with the reason
    recorded, and the next run retries.

Talks to jobs-api directly with an actor header (the gateway strips
inbound identity, same as boss-step.sh). Steps are addressed by
`spec_slug` with a title fallback for steps that predate the column.
"""

import datetime
import fcntl
import json
import os
import subprocess
import sys
import urllib.error
import urllib.request

JOBS = os.environ.get("BOSS_JOBS_URL", "http://127.0.0.1:7900")
GH_REPO = os.environ.get("BOSS_TRAIN_GH_REPO", "algedonic-dev/boss")
HEAD_OWNER = os.environ.get("BOSS_TRAIN_HEAD_OWNER", "dauld")
FORK_URL = os.environ.get("BOSS_TRAIN_FORK_URL", "https://github.com/dauld/boss-fork.git")
# The train protocol revision (directive 27ab7680): under the forge,
# CI-green trains merge themselves — GitHub was a 10-hour permission
# wall on an all-green train, and the human wall in this protocol is
# the car review at parking, not a mechanical click at landing.
AUTO_MERGE = os.environ.get("BOSS_TRAIN_AUTO_MERGE") == "1"
UPSTREAM_URL = os.environ.get("BOSS_TRAIN_UPSTREAM_URL", f"https://github.com/{GH_REPO}.git")
HOME = os.environ.get("BOSS_TRAIN_HOME", "/var/lib/boss-train")
CLONE = os.path.join(HOME, "repo")
DEPLOY_TREE = os.environ.get("BOSS_TRAIN_DEPLOY_TREE", "/opt/boss")
ACTOR = "automation:train-conductor"
BOSS_USER = json.dumps({
    "id": ACTOR, "role": "platform-admin", "access_tier": "operator",
    "territory_account_ids": [], "direct_report_ids": [], "department": "platform",
})

DRY = "--dry-run" in sys.argv
RECONCILE_ONLY = "--reconcile-only" in sys.argv
PREFLIGHT_ONLY = "--preflight" in sys.argv


def log(msg):
    print(f"conductor: {msg}", flush=True)


# Phase 0 — pre-flight the locomotive
#
# The 2026-08-10 18:01 window crashed before boarding: a sudo probe had
# left root-owned objects in the clone, and the conductor's fetch died
# at the moment the window opened. The consist had been rehearsed; the
# locomotive had not. Every entry (including the 10-minute reconcile,
# which is thereby the early-warning cadence) proves the clone healthy
# before touching train state, and a sick locomotive exits 3 — loud in
# the unit's status — instead of surfacing at departure time.

def preflight():
    """Return a list of problems; empty means the locomotive is fit."""
    problems = []
    if os.geteuid() == 0:
        # A root run is how the clone got poisoned in the first place:
        # everything it touches stops belonging to the service user.
        problems.append("running as root — the conductor owns its clone "
                        "and a root run leaves root-owned objects behind")
        return problems
    if not os.path.isdir(os.path.join(CLONE, ".git")):
        log("preflight: no clone yet — first boarding will create it")
        return problems
    me = os.geteuid()
    foreign = []
    for dirpath, _dirnames, filenames in os.walk(os.path.join(CLONE, ".git")):
        for name in filenames:
            p = os.path.join(dirpath, name)
            try:
                if os.lstat(p).st_uid != me:
                    foreign.append(p)
            except FileNotFoundError:
                continue  # gc'd mid-walk; ownership of what remains is what matters
    if foreign:
        shown = ", ".join(foreign[:3])
        problems.append(f"{len(foreign)} object(s) in the clone not owned "
                        f"by uid {me} (e.g. {shown}) — a foreign-uid run "
                        f"has poisoned {CLONE}")
    for remote in ("origin", "fork"):
        r = sh("git", "-C", CLONE, "fetch", remote, "--prune", "--dry-run",
               check=False)
        if r.returncode != 0:
            problems.append(f"dry fetch of {remote} failed: "
                            f"{r.stderr.strip().splitlines()[-1] if r.stderr.strip() else 'rc=' + str(r.returncode)}")
    return problems


def api(method, path, payload=None):
    req = urllib.request.Request(
        JOBS + path,
        method=method,
        headers={"content-type": "application/json", "x-boss-user": BOSS_USER},
        data=json.dumps(payload).encode() if payload is not None else None,
    )
    with urllib.request.urlopen(req, timeout=30) as r:
        body = r.read()
    return json.loads(body) if body.strip() else None


def sh(*args, cwd=None, check=True):
    r = subprocess.run(args, cwd=cwd, capture_output=True, text=True)
    if check and r.returncode != 0:
        raise RuntimeError(f"{' '.join(args)}: rc={r.returncode}\n{r.stderr.strip()}")
    return r


def rows(resp):
    return resp["data"] if isinstance(resp, dict) and "data" in resp else resp


def find_step(job, slug, title=None):
    for s in job.get("steps", []):
        if s.get("spec_slug") == slug:
            return s
    for s in job.get("steps", []):
        if title and s.get("title") == title:
            return s
    return None


def step_done(step):
    return step is not None and step.get("status") in ("completed", "skipped")


def complete_step(job, step, **fields):
    if step_done(step):
        return
    md = dict(step.get("metadata") or {})
    md.update({k: v for k, v in fields.items() if v is not None})
    if DRY:
        log(f"DRY: would complete {step.get('spec_slug') or step.get('title')} "
            f"on {job['id'][:8]} with {fields}")
        return
    api("PUT", f"/api/jobs/{job['id']}/steps/{step['id']}",
        {"status": "completed", "metadata": md})
    log(f"completed {step.get('spec_slug') or step.get('title')} on {job['id'][:8]}")


def merge_job_metadata(job_id, **kv):
    """update_job takes a whole Job; fetch, merge metadata, put back."""
    job = api("GET", f"/api/jobs/{job_id}")
    job["metadata"] = {**(job.get("metadata") or {}), **kv}
    if DRY:
        log(f"DRY: would set {list(kv)} on job {job_id[:8]}")
        return job
    api("PUT", f"/api/jobs/{job_id}", job)
    return job


# ---------------------------------------------------------------------------
# Phase 1 — reconcile open trains against reality
# ---------------------------------------------------------------------------

# ---------------------------------------------------------------------------
# The forge seam (internal-forge.md Q7a): every talk-to-the-code-host
# call goes through Forge, so internalizing Git/CI is an adapter swap
# — a ForgejoForge sibling selected by BOSS_TRAIN_FORGE — instead of
# a conductor rewrite at cutover. The GitHub adapter shells to `gh`
# exactly as before; behavior is unchanged by this refactor.
# ---------------------------------------------------------------------------

class GitHubForge:
    """The code host as the conductor sees it: three verbs."""

    def pr_info(self, url):
        """-> {state, mergeCommit, statusCheckRollup} for a PR url."""
        r = sh("gh", "pr", "view", url,
               "--json", "state,mergeCommit,statusCheckRollup")
        return json.loads(r.stdout)

    def pr_create(self, repo, head_branch, title, body):
        """Open a PR head->main on repo; return its url."""
        r = sh("gh", "pr", "create", "--repo", repo,
               "--head", f"{HEAD_OWNER}:{head_branch}", "--base", "main",
               "--title", title, "--body", body)
        return r.stdout.strip().splitlines()[-1]

    def merge(self, url):
        sh("gh", "pr", "merge", url, "--squash")


class ForgejoForge:
    """The same three verbs against the internal forge's API. PRs are
    same-repo (no fork dance): the train branch pushes to the one
    repo and the PR head is the bare branch name."""

    def __init__(self):
        self.base = os.environ.get(
            "BOSS_TRAIN_FORGE_URL", "http://10.20.0.15:3000").rstrip("/")
        self.repo = os.environ.get("BOSS_TRAIN_FORGE_REPO", "david/boss")
        token_file = os.environ.get(
            "BOSS_TRAIN_FORGE_TOKEN_FILE", "/etc/boss-train/forge.token")
        with open(token_file) as f:
            self.token = f.read().strip()

    def _api(self, method, path, payload=None):
        req = urllib.request.Request(
            f"{self.base}/api/v1{path}", method=method,
            headers={"Authorization": f"token {self.token}",
                     "Content-Type": "application/json"},
            data=json.dumps(payload).encode() if payload is not None else None)
        with urllib.request.urlopen(req, timeout=30) as r:
            body = r.read()
        return json.loads(body) if body.strip() else None

    def _index(self, url):
        return url.rstrip("/").rsplit("/", 1)[-1]

    def pr_info(self, url):
        """Shape Forgejo's PR + combined status into the exact dict
        the GitHub adapter returns, so reconcile stays forge-blind."""
        idx = self._index(url)
        pr = self._api("GET", f"/repos/{self.repo}/pulls/{idx}")
        state = "MERGED" if pr.get("merged") else (
            "OPEN" if pr.get("state") == "open" else "CLOSED")
        head_sha = (pr.get("head") or {}).get("sha", "")
        rollup = []
        if head_sha:
            combined = self._api(
                "GET", f"/repos/{self.repo}/commits/{head_sha}/status")
            for st in (combined or {}).get("statuses", []) or []:
                verdict = (st.get("status") or "").lower()
                rollup.append({
                    "conclusion": {"success": "SUCCESS",
                                   "failure": "FAILURE",
                                   "error": "FAILURE"}.get(verdict, ""),
                    "status": "PENDING" if verdict == "pending" else "COMPLETED",
                })
        return {
            "state": state,
            "mergeCommit": {"oid": pr.get("merge_commit_sha") or ""},
            "statusCheckRollup": rollup,
        }

    def pr_create(self, repo, head_branch, title, body):
        pr = self._api("POST", f"/repos/{self.repo}/pulls", {
            "head": head_branch, "base": "main",
            "title": title, "body": body,
        })
        return pr["html_url"]

    def merge(self, url):
        idx = self._index(url)
        self._api("POST", f"/repos/{self.repo}/pulls/{idx}/merge",
                  {"Do": "squash"})


def make_forge():
    kind = os.environ.get("BOSS_TRAIN_FORGE", "github")
    if kind == "github":
        return GitHubForge()
    if kind == "forgejo":
        return ForgejoForge()
    raise RuntimeError(f"unknown BOSS_TRAIN_FORGE {kind!r} — "
                       "expected github or forgejo")


FORGE = make_forge()


def gh_pr(url):
    return FORGE.pr_info(url)


def ci_verdict(rollup):
    """Collapse gh's per-check rollup to green/pending/failing."""
    if not rollup:
        return "pending"
    states = {(c.get("conclusion") or c.get("status") or "").upper() for c in rollup}
    if states & {"FAILURE", "TIMED_OUT", "CANCELLED", "ACTION_REQUIRED"}:
        return "failing"
    if states - {"SUCCESS", "NEUTRAL", "SKIPPED", "COMPLETED"}:
        return "pending"
    return "green"


def deploy(train, deployed_step):
    """Carry a merged train out to the playground — only from a clean
    main tree; anything else is recorded and retried next run."""
    tree = DEPLOY_TREE
    dirty = sh("git", "-C", tree, "status", "--porcelain", check=False).stdout.strip()
    branch = sh("git", "-C", tree, "rev-parse", "--abbrev-ref", "HEAD").stdout.strip()
    if dirty or branch != "main":
        reason = f"deploy tree busy (branch={branch}, dirty={bool(dirty)}) — will retry"
        log(reason)
        if not DRY:
            md = dict(deployed_step.get("metadata") or {})
            md["deploy_blocked"] = reason
            api("PUT", f"/api/jobs/{train['id']}/steps/{deployed_step['id']}",
                {"metadata": md})
        return
    if DRY:
        log("DRY: would pull main, migrate, build, deploy services + web")
        return
    # Under the forge protocol the playground converges on forge
    # main; GitHub is the mirror, never the source (27ab7680).
    pull_remote = os.environ.get("BOSS_TRAIN_DEPLOY_REMOTE", "origin")
    sh("git", "-C", tree, "pull", pull_remote, "main")
    main_ref = sh("git", "-C", tree, "rev-parse", "--short", "HEAD").stdout.strip()
    env = {**os.environ, "PGPASSWORD": "boss"}
    mig = subprocess.run(
        [f"{tree}/infra/postgres/migrate.sh", "--",
         "psql", "-U", "boss", "-h", "127.0.0.1", "-d", "boss"],
        cwd=tree, capture_output=True, text=True, env=env)
    if mig.returncode != 0:
        raise RuntimeError(f"migrate.sh failed:\n{mig.stderr.strip()}")
    sh(f"{tree}/infra/build-release.sh", cwd=tree)
    sh("sudo", "-n", f"{tree}/infra/deploy-services.sh", "prod", cwd=tree)
    sh("sudo", "-n", f"{tree}/infra/deploy-web.sh", cwd=tree)
    summary = (f"main@{main_ref}; {mig.stdout.strip().splitlines()[-1]}; "
               f"services: prod; web: deployed")
    complete_step(train, deployed_step, deployed=summary)


def reconcile():
    trains = rows(api("GET", "/api/jobs?kind=pr-train&status=open&limit=50"))
    for t in trains:
        t = api("GET", f"/api/jobs/{t['id']}")
        pr_step = find_step(t, "pr", "Open the batched PR")
        if not step_done(pr_step):
            continue  # this window's board phase, or a stalled assembly
        pr_url = (pr_step.get("metadata") or {}).get("pr_url")
        if not pr_url:
            continue
        info = gh_pr(pr_url)

        ci_step = find_step(t, "ci", "CI verdict")
        verdict = ci_verdict(info.get("statusCheckRollup"))
        if not step_done(ci_step) and verdict != "pending":
            complete_step(t, ci_step, result=verdict)

        if AUTO_MERGE and verdict == "green" and info.get("state") == "OPEN":
            log(f"CI green — merging {pr_url} (train protocol 27ab7680)")
            if not DRY:
                FORGE.merge(pr_url)
                info = gh_pr(pr_url)

        merged_step = find_step(t, "merged", "Merged into main")
        if info.get("state") == "MERGED" and not step_done(merged_step):
            merge_ref = (info.get("mergeCommit") or {}).get("oid", "unknown")[:12]
            complete_step(t, merged_step, merge_ref=merge_ref)
            for cid in (t.get("metadata") or {}).get("boarded_jobs", []):
                # v3 ship-a-change gates `merged` on this marker; the
                # dispatcher closes the Job once it is set.
                merge_job_metadata(cid, merged="true", merge_ref=merge_ref)
            t = api("GET", f"/api/jobs/{t['id']}")

        merged_step = find_step(t, "merged", "Merged into main")
        deployed_step = find_step(t, "deployed", "Deployed to the playground")
        if step_done(merged_step) and not step_done(deployed_step):
            deploy(t, deployed_step)


# ---------------------------------------------------------------------------
# Phase 2 — board this window's train
# ---------------------------------------------------------------------------

def ensure_clone():
    if not os.path.isdir(os.path.join(CLONE, ".git")):
        os.makedirs(HOME, exist_ok=True)
        sh("git", "clone", UPSTREAM_URL, CLONE)
        sh("git", "-C", CLONE, "remote", "add", "fork", FORK_URL)
        # The merge commits the assembly makes need an author, and the
        # honest one is the machine that made them (a fresh clone has
        # no identity — the first real run failed exactly here).
        sh("git", "-C", CLONE, "config", "user.name", "BOSS train conductor")
        sh("git", "-C", CLONE, "config", "user.email", "train-conductor@boss.invalid")
    sh("git", "-C", CLONE, "fetch", "origin", "--prune")
    sh("git", "-C", CLONE, "fetch", "fork", "--prune")


def candidates():
    out = []
    for j in rows(api("GET", "/api/jobs?kind=ship-a-change&status=open&limit=100")):
        j = api("GET", f"/api/jobs/{j['id']}")
        md = j.get("metadata") or {}
        branch = md.get("branch")
        if not branch or md.get("train") or branch.startswith("train/"):
            continue
        review = find_step(j, "review", "Open for review")
        if review is None or review.get("status") not in ("ready", "active"):
            continue
        ok = sh("git", "-C", CLONE, "rev-parse", "--verify", "--quiet",
                f"fork/{branch}", check=False)
        if ok.returncode != 0:
            log(f"{j['id'][:8]}: branch {branch} not on fork — leaving behind")
            if not DRY:
                # Loud on the Job, not just in the journal: the author
                # parked this at review believing it would board.
                merge_job_metadata(j["id"], skip_reason=f"branch {branch} "
                                   "not found on the fork — push it, then "
                                   "the next train boards it")
            continue
        out.append((j, branch))
    return out


def open_train_job(train_branch, window):
    payload = {
        "kind": "pr-train",
        "subject": {"subject_kind": "custom", "id": train_branch},
        "title": f"PR train {window}",
        "owner_id": "emp-bootstrap-admin",
        "status": "open",
        "priority": "standard",
        "metadata": {"actor": ACTOR},
        "tags": ["train"],
    }
    if DRY:
        log(f"DRY: would open train Job for {train_branch}")
        return None
    created = api("POST", "/api/jobs", payload)
    jid = created["id"] if isinstance(created, dict) and "id" in created else None
    if jid is None:  # some create paths return the row wrapped
        jid = rows(api("GET", "/api/jobs?kind=pr-train&status=open&limit=5"))[0]["id"]
    return api("GET", f"/api/jobs/{jid}")


def board(now):
    ensure_clone()
    cands = candidates()
    window = now.strftime("%Y-%m-%d ") + ("AM" if now.hour < 12 else "PM")
    train_branch = "train/" + now.strftime("%Y%m%d-%H%M")
    train = open_train_job(train_branch, window)
    if train is None:  # dry run
        log(f"DRY: candidates: {[(j['id'][:8], b) for j, b in cands]}")
        return
    collect = find_step(train, "collect", "Collect what is ready to board")

    if not cands:
        merge_job_metadata(train["id"], empty="true")
        complete_step(train, collect, boarded="nothing ready to board this window")
        log("empty window — train cancels via the marker")
        return

    sh("git", "-C", CLONE, "checkout", "-B", train_branch, "origin/main")
    boarded, skipped = [], []
    for j, branch in cands:
        r = sh("git", "-C", CLONE, "merge", "--no-ff", "-m",
               f"train: merge {branch}", f"fork/{branch}", check=False)
        if r.returncode == 0:
            boarded.append((j, branch))
        else:
            conflicted = sh("git", "-C", CLONE, "diff", "--name-only",
                            "--diff-filter=U", check=False).stdout.split()
            sh("git", "-C", CLONE, "merge", "--abort", check=False)
            skipped.append((j, branch))
            files = ", ".join(conflicted) or "unresolved (merge died before conflict markers)"
            merge_job_metadata(j["id"], skip_reason=f"merge conflict with "
                               f"this window's train in: {files} — rebase "
                               "onto main and repark at review")
            log(f"conflict merging {branch} ({files}) — left for the next train")

    if not boarded:
        merge_job_metadata(train["id"], empty="true")
        complete_step(train, collect, boarded="all candidates skipped on merge conflicts: "
                      + ", ".join(b for _, b in skipped))
        return

    sh("git", "-C", CLONE, "push", "fork", train_branch)
    train_ref = sh("git", "-C", CLONE, "rev-parse", "--short", "HEAD").stdout.strip()

    lines = [f"- `{b}` — {j['title']} (Job `{j['id'][:8]}`)" for j, b in boarded]
    if skipped:
        lines.append("")
        lines.append("Left behind on merge conflicts (next train): "
                     + ", ".join(b for _, b in skipped))
    body = (f"The {window} train: {len(boarded)} change(s) batched by the conductor.\n\n"
            + "\n".join(lines)
            + "\n\n🤖 opened by infra/train/conductor.py (pr-train Workflow)")
    pr_url = FORGE.pr_create(
        GH_REPO, train_branch,
        f"train: {window} ({len(boarded)} changes)", body)

    merge_job_metadata(train["id"],
                       boarded_jobs=[j["id"] for j, _ in boarded],
                       skipped_branches=[b for _, b in skipped])
    train = api("GET", f"/api/jobs/{train['id']}")
    complete_step(train, find_step(train, "collect", "Collect what is ready to board"),
                  boarded=", ".join(f"{b} ({j['id'][:8]})" for j, b in boarded))
    complete_step(train, find_step(train, "assemble", "Assemble the train branch"),
                  train_ref=f"{train_branch}@{train_ref}",
                  skipped=", ".join(b for _, b in skipped) or "none")
    complete_step(train, find_step(train, "pr", "Open the batched PR"), pr_url=pr_url)

    for j, branch in boarded:
        review = find_step(j, "review", "Open for review")
        complete_step(j, review, pr_url=pr_url,
                      note=f"boarded train {train['id'][:8]} ({train_branch})")
        # skip_reason cleared on boarding: an earlier window's skip note
        # must not outlive the skip.
        merge_job_metadata(j["id"], train=train["id"], skip_reason="")
    log(f"train {train['id'][:8]} boarded {len(boarded)}, PR {pr_url}")


def main():
    os.makedirs(HOME, exist_ok=True)
    lock = open(os.path.join(HOME, "lock"), "w")
    try:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except BlockingIOError:
        # A held lock means a conductor run is active right now — the
        # locomotive is demonstrably pulling; a standalone pre-flight
        # has nothing further to prove.
        log("another conductor run holds the lock — leaving")
        return 0
    problems = preflight()
    if problems:
        for p in problems:
            log(f"preflight FAIL: {p}")
        return 3
    log("preflight ok")
    if PREFLIGHT_ONLY:
        return 0
    reconcile()
    if not RECONCILE_ONLY:
        board(datetime.datetime.now(datetime.timezone.utc))
    return 0


if __name__ == "__main__":
    sys.exit(main())
