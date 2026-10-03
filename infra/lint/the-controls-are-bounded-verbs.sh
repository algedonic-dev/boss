#!/usr/bin/env bash
# the-controls-are-bounded-verbs.sh — the ad hoc controls are ops verbs
# with the reclaim-disk shape: a script in the tree, params with
# patterns that admit no whitespace and no leading dash, a required
# sha for the rollback; the hold file round-trips; the runner's hold
# check reads it.
set -uo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"; repo="$(cd "$here/../.." && pwd)"
fail() { echo "FAIL: $*" >&2; exit 1; }
# The allowlist is the directory infra/ops/verbs/, assembled by the one
# script the runner itself uses (5086842d) — read here the same way.
# BOUNDED_VERBS_DIR is the tests' seam (github_act_sh.rs plants a verb
# that spells its script another way); the gate reads the tree's own.
allowlist="$(sh "$repo/infra/ops/verbs-allowlist.sh" "${BOUNDED_VERBS_DIR:-$repo/infra/ops/verbs}")" \
    || fail "infra/ops/verbs-allowlist.sh could not assemble infra/ops/verbs/ (see above)"
# On a file descriptor, never an argv word: one word is capped at 128 KiB
# (MAX_ARG_STRLEN), and the assembled allowlist crossed that on
# 2026-09-28 with the three GitHub verbs (backlog 6a8ff89f).
python3 - "$repo" <(printf '%s' "$allowlist") <<'PY' || exit 1
import json,re,sys,os
repo=sys.argv[1]; v=json.load(open(sys.argv[2]))["verbs"]
# THE ROSTER IS DERIVED, not listed here. It used to be four names typed
# into this loop, which meant every mutating verb added after them —
# reclaim-disk, converge, mirror-base-images, delete-orphan-object — was
# outside the only check that says a mutating verb is bounded and
# authorized. A roster that has to be edited in two places is the §9a
# defect; a verb that declares itself MUTATING is the one definition.
mutating=sorted(n for n,s in v.items() if "MUTATING" in s.get("about",""))
len(mutating) >= 8 or sys.exit(f"FAIL: only {len(mutating)} verb(s) declare MUTATING — the roster derivation broke: {mutating}")
# The scanned line every scanner prints (infra/lint/lib/scanned.sh),
# from here because the count lives in this program; the floor above
# is its refusal on zero.
print(f"the-controls-are-bounded-verbs: scanned {len(mutating)} MUTATING verb(s) of {len(v)} under infra/ops/verbs")
for name in mutating:
    spec=v[name]
    # argv[0] is repo-relative (66077f9c); the runner resolves it against
    # its own checkout, and so does this lint — one rule, no substitution.
    argv0=spec["argv"][0]
    script=argv0 if argv0.startswith("/") else os.path.join(repo, argv0)
    os.path.isfile(script) or sys.exit(f"FAIL: {name} points at a script not in the tree: {spec['argv'][0]}")
    os.access(script, os.X_OK) or sys.exit(f"FAIL: {name}'s script is not executable")
    for p in spec["params"]:
        if "one_of" in p:
            # A literal list is exact-match only, and each word is
            # reviewed file content: whitespace-free, so the runner's
            # newline-split argv stays exact. A leading dash is allowed
            # HERE precisely because the packet cannot supply the word.
            "pattern" in p and sys.exit(f"FAIL: {name}.{p['name']} mixes one_of with a pattern — a literal list is equality only")
            words=p["one_of"]
            (isinstance(words,list) and words and all(isinstance(w,str) and w and not re.search(r"\s",w) for w in words)) \
                or sys.exit(f"FAIL: {name}.{p['name']}.one_of must be a non-empty list of whitespace-free words")
            continue
        pat=p["pattern"]
        for bad in (" ", "\t", "\n"):
            re.fullmatch(pat, "a"+bad+"b") and sys.exit(f"FAIL: {name}.{p['name']} admits whitespace")
        re.fullmatch(pat, "-x") and sys.exit(f"FAIL: {name}.{p['name']} admits a leading dash")
    "MUTATING" in spec["about"] or sys.exit(f"FAIL: {name} does not say it is MUTATING")
    "David" in spec["about"] or sys.exit(f"FAIL: {name} names no authorization")
rb=v["rollback-to"]["params"][0]
"default" in rb and sys.exit("FAIL: rollback-to's sha has a default — a rollback must name its target")
re.fullmatch(rb["pattern"],"2683908") or sys.exit("FAIL: a 7-char sha is refused")
re.fullmatch(rb["pattern"],"b2814ef") or sys.exit("FAIL: a real short sha is refused")
re.fullmatch(rb["pattern"],"latest") and sys.exit("FAIL: 'latest' passes as a sha")
# publish-github-pr takes NO packet-supplied TEXT (fixed repos, a dated
# branch): its one param is a literal list the packet can only select
# from, and the only literal is --check — the verb's own no-network
# input check (6964f9e8), so the verb is exercisable through the runner
# without the real run; optional, because the real run passes no arg.
# It declares its own timeout, because a first push of the whole tree
# exceeds the runner's 30s default — and the runner must actually read
# that field, or the number is decoration.
pub=v["publish-github-pr"]
free=[p["name"] for p in pub["params"] if "one_of" not in p]
free and sys.exit(f"FAIL: publish-github-pr must take no packet-supplied text (pattern params: {free})")
lits=sorted(w for p in pub["params"] for w in p["one_of"])
lits==["--check"] or sys.exit(f"FAIL: publish-github-pr must admit exactly the literal --check, got {lits}")
all(p.get("optional") is True for p in pub["params"]) or sys.exit("FAIL: publish-github-pr's --check must be optional — the real run passes no arg")
isinstance(pub.get("timeout"), int) and pub["timeout"] >= 120 or sys.exit("FAIL: publish-github-pr must declare a timeout of at least 120s")
# merge-publish-pr (backlog 602fe95f) writes the public mirror's MAIN: the
# same script with --merge FIXED in its argv and no param at all, so a
# rule that files it can file a merge and nothing else, and the packet
# can hand it no text. The approval it acts on is the publish packet's
# passkey, which the script re-reads; the bound is the argv.
mrg=v.get("merge-publish-pr") or sys.exit("FAIL: merge-publish-pr is not in infra/ops/verbs/ — the publish's merge has no bounded verb")
mrg["argv"]==["infra/forge/publish-github-pr.sh","--merge"] or sys.exit(f"FAIL: merge-publish-pr must run exactly infra/forge/publish-github-pr.sh --merge, got {mrg['argv']}")
mrg["params"]==[] or sys.exit(f"FAIL: merge-publish-pr must take no packet-supplied input, got params {[p.get('name') for p in mrg['params']]}")
"merge-publish-pr" in mutating or sys.exit("FAIL: merge-publish-pr writes main and must say MUTATING")
isinstance(mrg.get("timeout"), int) and mrg["timeout"] >= 120 or sys.exit("FAIL: merge-publish-pr must declare a timeout of at least 120s")
# The GitHub acts (design 76155676 decision 4, backlog 6a8ff89f): every
# verb that runs infra/forge/github-act.sh is EITHER its plan — a
# read-only `--plan` render — OR a MUTATING write that runs only under a
# passkey-signed plan. Derived from the argv, never a list here, so a
# fourth act inherits the rule. And none takes a credential or a path
# from the packet: the installation is the request's owner, and its
# token slot is the script's to derive.
#
# A verb is a GitHub verb when ANY argv word reaches the script — by name,
# or by a path that resolves to it (`infra/forge/./github-act.sh`,
# `infra/ops/../forge/…`, a wrapping `bash`) — and every such verb must
# then name it EXACTLY as argv[0]. The adversarial review of 78959555 (M2)
# found the literal argv[0] match let those spellings slip past, and the
# runner strips a filer-supplied plan_sha256 only for a requires_approval
# verb, so an unapproved verb carrying a hash would have written unsigned.
GHA="infra/forge/github-act.sh"
gha_real=os.path.realpath(os.path.join(repo, GHA))
ACTS=("create-repository","set-branch-protection","delete-refs","disable-actions")
def reaches_gha(argv):
    return any("github-act" in a or os.path.realpath(os.path.join(repo, a))==gha_real for a in argv)
ghv=sorted(n for n,s in v.items() if reaches_gha(s["argv"]))
ghv or sys.exit(f"FAIL: no verb runs {GHA} — the GitHub acts' derivation broke")
for n in ghv:
    s=v[n]; argv=s["argv"]
    argv[0]==GHA or sys.exit(f"FAIL: {n} reaches {GHA} as {argv[:2]} — a GitHub verb names the script exactly as argv[0], so every reader of the allowlist sees what it runs")
    argv[1:2] and argv[1] in ACTS or sys.exit(f"FAIL: {n} runs {GHA} with act {argv[1:2]} — the acts are {', '.join(ACTS)}")
    if "--plan" in argv:
        argv[2:3]==["--plan"] or sys.exit(f"FAIL: {n} carries --plan somewhere other than right after the act")
        n in mutating and sys.exit(f"FAIL: {n} says MUTATING and passes --plan")
        s.get("requires_approval") and sys.exit(f"FAIL: {n} is a --plan render that requires approval — a plan must be renderable before anything is signed")
    else:
        n in mutating or sys.exit(f"FAIL: {n} runs {GHA} without MUTATING and without --plan — a verb that is not a plan must say it writes")
        s.get("requires_approval") is True or sys.exit(f"FAIL: {n} acts on GitHub without requires_approval — a GitHub write runs only under a signed plan")
    names=[p["name"] for p in s["params"]]
    names[:1]==["owner"] or sys.exit(f"FAIL: {n}'s first param must be owner, the installation it acts as: {names}")
    [x for x in names if any(k in x for k in ("token","path","file","url"))] and sys.exit(f"FAIL: {n} takes a credential, a path or a URL from the packet: {names}")
print(f"verbs: {len(ghv)} GitHub verb(s) run github-act.sh, each a --plan render or an approval-gated write")
# delete-orphan-object: the one verb whose authority is DERIVED rather
# than granted. There is deliberately no general `kubectl delete` verb,
# so the properties that keep this one bounded are the properties that
# keep the allowlist worth having: one object named in full, the kind
# floor, and the derivation it re-runs at call time.
dob=v["delete-orphan-object"]
pats=[p for p in dob["params"] if "one_of" not in p]
[p["name"] for p in pats]==["object"] or sys.exit(f"FAIL: delete-orphan-object must take exactly one pattern param, `object`, got {[p['name'] for p in pats]}")
lits=sorted(w for p in dob["params"] if "one_of" in p for w in p["one_of"])
lits==["--dry-run"] or sys.exit(f"FAIL: delete-orphan-object must admit exactly the literal --dry-run, got {lits}")
all(p.get("optional") is True for p in dob["params"] if "one_of" in p) or sys.exit("FAIL: --dry-run must be optional — the real run passes no second arg")
op=pats[0]["pattern"]
"default" in pats[0] and sys.exit("FAIL: delete-orphan-object's object has a default — a delete must name its target")
for ok in ("Service/boss/boss-docs-internal","ConfigMap/boss-dev/gate-runner-script"):
    re.fullmatch(op, ok) or sys.exit(f"FAIL: the object pattern refuses {ok}")
# A delete whose target is not fully named is a delete with a scope, and
# a scope is what this verb must never accept.
for bad in ("services/boss/x","Service/boss","boss-docs-internal","Service/boss/x/y","Service//x","*/boss/x","Service/boss/*"):
    re.fullmatch(op, bad) and sys.exit(f"FAIL: the object pattern admits {bad}")
isinstance(dob.get("timeout"), int) and dob["timeout"] >= 120 or sys.exit("FAIL: delete-orphan-object must declare a timeout — the derivation parses every manifest")
for phrase in ("undeclared-objects.sh","DERIVED","--dry-run"):
    phrase in dob["about"] or sys.exit(f"FAIL: delete-orphan-object's about does not say {phrase}")
for n,s in v.items():
    re.search(r"\bkubectl\b[^.\n]*\bdelete\b", " ".join(s["argv"])) and sys.exit(f"FAIL: verb {n} hands kubectl a delete directly — the allowlist must not carry an unbounded delete")
# The derivation is the authority, so it has to be there, and the verb
# has to be the thing that calls it.
derive=f"{repo}/infra/cluster/undeclared-objects.sh"
os.path.isfile(derive) and os.access(derive, os.X_OK) or sys.exit("FAIL: infra/cluster/undeclared-objects.sh is missing or not executable")
dobsh=open(f"{repo}/infra/forge/delete-orphan-object.sh").read()
"undeclared-objects.sh" in dobsh or sys.exit("FAIL: delete-orphan-object.sh does not call the derivation")
re.search(r'delete "\$KIND" "\$NAME" -n "\$NS"', dobsh) or sys.exit("FAIL: delete-orphan-object.sh's delete does not use the derivation's own fields")
'"$TARGET"' in dobsh.split("--- the delete")[-1] and sys.exit("FAIL: delete-orphan-object.sh's delete reads the packet's string instead of the derivation's answer")
floor=re.search(r"^DELETABLE_KINDS=\(([^)]*)\)", dobsh, re.M) or sys.exit("FAIL: delete-orphan-object.sh declares no DELETABLE_KINDS floor")
kinds=floor.group(1).split()
# Bytes, credentials and privileges stay a named human step. A widening
# here is a reviewed change to that file, and this is the review.
for withheld in ("PersistentVolumeClaim","StatefulSet","Secret","ServiceAccount","Role","RoleBinding","Namespace","ClusterRole","ClusterRoleBinding"):
    withheld in kinds and sys.exit(f"FAIL: the kind floor admits {withheld} — bytes, credentials and privileges stay a human step")
kinds or sys.exit("FAIL: the kind floor is empty")
print(f"verbs: the kind floor is {' '.join(kinds)}; the object pattern names one object in full")

# THE NODE VERBS (backlog f0aaa72f; design 8457c07b): every verb whose
# script sources infra/forge/cluster-node-lib.sh — derived from the
# script, never a list here — acts on ONE node the estate registry names,
# through the forge's admin doors. So: its first param is a required
# `node` that is one DNS label (never a comma list, an address or a
# flag); it serves the forge only, the host that holds the credentials;
# a MUTATING one's script refuses a non-worker (node_require_worker)
# BEFORE it opens a door (node_door); the shutdown runs only under a
# signed plan; and no node verb hands talosctl a free resource or an
# unbounded subcommand.
LIB="cluster-node-lib.sh"
CONVERGE="infra/forge/node-converge.sh"
def script_text(spec):
    a0=spec["argv"][0]
    p=a0 if a0.startswith("/") else os.path.join(repo, a0)
    return open(p).read() if os.path.isfile(p) else ""
nodev=sorted(n for n,s in v.items() if f"/{LIB}" in script_text(s))
len(nodev) >= 5 or sys.exit(f"FAIL: only {len(nodev)} verb(s) run a script that sources {LIB} — the node-verb derivation broke: {nodev}")
for n in nodev:
    s=v[n]; ps=s["params"]
    s.get("hosts")==["forge"] or sys.exit(f"FAIL: node verb {n} serves {s.get('hosts')} — the admin kubeconfig and talosconfig live on the forge only")
    ps and ps[0]["name"]=="node" or sys.exit(f"FAIL: node verb {n}'s first param must be `node`: {[p['name'] for p in ps]}")
    p0=ps[0]
    ("default" in p0 or p0.get("optional")) and sys.exit(f"FAIL: node verb {n}'s node has a default or is optional — a node verb names its node")
    for ok in ("w-1","w-2","cp-1","worker10"):
        re.fullmatch(p0["pattern"], ok) or sys.exit(f"FAIL: node verb {n}'s node pattern refuses {ok}")
    for bad in ("w-1,cp-1","10.0.0.1","W-1","-w","w-1-","w.1","w 1",""):
        re.fullmatch(p0["pattern"], bad) and sys.exit(f"FAIL: node verb {n}'s node pattern admits {bad!r}")
    for w in s["argv"]:
        w in ("--force","machineconfig","secrets","reset","reboot","upgrade","apply-config","edit","patch") and sys.exit(f"FAIL: node verb {n} carries {w!r} in its argv")
    body=script_text(s)
    if n in mutating:
        code="\n".join(l for l in body.splitlines() if not l.lstrip().startswith("#"))
        d=code.find("node_door")
        if s["argv"][0]==CONVERGE:
            # node-converge (backlog 9d56c616) is the ONE mutating node
            # verb that may act on a control plane: design 1bc4b4ed
            # converges every declared node and bounds the ACT instead —
            # a Talos node (node_require_talos) before any door, the
            # cluster agreeing on its kind, every talosctl patch in
            # --mode=no-reboot and nothing else, under a signed plan.
            g=code.find("node_require_talos ")
            (g >= 0 and d >= 0 and g < d) or sys.exit(f"FAIL: node verb {n}'s script does not refuse a non-Talos node (node_require_talos) before it opens a door (node_door)")
            "node_agree_cluster " in code or sys.exit(f"FAIL: node verb {n}'s script does not hold the registry's kind of node to the cluster's (node_agree_cluster)")
            modes=re.findall(r"--mode[= ]([A-Za-z-]+)", code)
            (modes and all(m=="no-reboot" for m in modes)) or sys.exit(f"FAIL: node verb {n}'s script must patch in --mode=no-reboot and no other mode, got {modes}")
            # Every talosctl call is `get machineconfig` (the read-back) or
            # `patch machineconfig` — never reboot, upgrade, reset, etcd,
            # edit or apply-config of a whole file.
            calls=re.findall(r'\$T[A-Z]*\s+-n\s+"\$NODE_ADDRESS"\s+(\S+)\s+(\S+)', code)
            (calls and all(c in (("get","machineconfig"),("patch","machineconfig")) for c in calls)) \
                or sys.exit(f"FAIL: node verb {n}'s script hands talosctl something other than get/patch machineconfig: {calls}")
            s.get("requires_approval") is True or sys.exit(f"FAIL: node verb {n} changes a node's machine config and must run only under a passkey-signed plan")
        else:
            g=code.find("node_require_worker ")
            (g >= 0 and d >= 0 and g < d) or sys.exit(f"FAIL: node verb {n}'s script does not refuse a non-worker (node_require_worker) before it opens a door (node_door)")
        "--force" in code and sys.exit(f"FAIL: node verb {n}'s script passes --force somewhere")
    for p in ps[1:]:
        if p["name"]=="resource":
            set(p.get("one_of") or ["<pattern>"]) <= {"machinestatus","disks","volumestatus","discoveredvolumes"} \
                or sys.exit(f"FAIL: node verb {n}'s resource must be a one_of of read-only status resources (never machineconfig or secrets), got {p.get('one_of') or p.get('pattern')}")
sd=v["shutdown-node"]
sd.get("requires_approval") is True and sd.get("plan_verb")=="plan-a-node-shutdown" \
    or sys.exit("FAIL: shutdown-node takes a node down and must run only under a passkey-signed plan-a-node-shutdown")
"--plan" in v["plan-a-node-shutdown"]["argv"] and "plan-a-node-shutdown" not in mutating \
    or sys.exit("FAIL: plan-a-node-shutdown must be the read-only --plan render")
nc=v["node-converge"]
nc.get("requires_approval") is True and nc.get("plan_verb")=="plan-a-node-converge" \
    or sys.exit("FAIL: node-converge changes a node's machine config and must run only under a passkey-signed plan-a-node-converge")
"--plan" in v["plan-a-node-converge"]["argv"] and "plan-a-node-converge" not in mutating \
    or sys.exit("FAIL: plan-a-node-converge must be the read-only --plan render")
# The patch is the tree's file at a named commit: a full sha, never a
# path or patch text from the packet.
[p["name"] for p in nc["params"]]==["node","tree_sha","plan_sha256"] \
    or sys.exit(f"FAIL: node-converge must take exactly node, tree_sha and plan_sha256, got {[p['name'] for p in nc['params']]}")
nc["params"][1].get("pattern")=="^[0-9a-f]{40}$" or sys.exit("FAIL: node-converge's tree_sha must be a full 40-hex commit")
print(f"verbs: {len(nodev)} node verb(s) ({', '.join(nodev)}) take one named node on the forge; the MUTATING ones refuse a non-worker before any door opens, node-converge a non-Talos node, under a signed no-reboot plan")

runner=open(f"{repo}/infra/ops/ops-runner.sh").read()
"$spec.timeout" in runner and "verb_timeout" in runner or sys.exit("FAIL: ops-runner.sh does not honour a verb's declared timeout")
"one_of" in runner and "optional" in runner or sys.exit("FAIL: ops-runner.sh does not read one_of/optional params — the literal allowlist is decoration")
print("verbs: " + ", ".join(mutating) + " are bounded and authorized")
PY
tmp="$(mktemp -d)"; trap 'rm -rf "$tmp"' EXIT
export BOSS_CONVERGE_HOLD="$tmp/hold"
# Under the ops runner a verb runs as root with NO HOME — the scripts
# must not need one (2026-09-05: "HOME: unbound variable").
env -i PATH="$PATH" BOSS_CONVERGE_HOLD="$tmp/hold" bash "$repo/infra/forge/converge-hold.sh" hold no-home-here >/dev/null || fail "converge-hold.sh needs HOME (the ops runner has none)"
env -i PATH="$PATH" BOSS_CONVERGE_HOLD="$tmp/hold" bash "$repo/infra/forge/converge-hold.sh" release >/dev/null || fail "release needs HOME"
env -i PATH="$PATH" bash -n "$repo/infra/forge/rollback-to.sh" || fail "rollback-to.sh does not parse"
# The node verbs (backlog f0aaa72f) run as root with no HOME too; their
# behaviour is boss-testing/tests/node_maintenance_verbs_sh.rs.
for s in cluster-node-lib.sh cordon-node.sh shutdown-node.sh talos-get.sh node-status.sh node-converge.sh; do
    env -i PATH="$PATH" bash -n "$repo/infra/forge/$s" || fail "$s does not parse"
    grep -qE '\$HOME' <<<"$(grep -vE '^\s*#' "$repo/infra/forge/$s")" && fail "$s reads \$HOME (the ops runner has none)"
done
# Code lines only — a comment may name $HOME to say why it is not used.
grep -qE '\$HOME' <<<"$(grep -vE '^\s*#' "$repo/infra/forge/rollback-to.sh")" && fail "rollback-to.sh still reads \$HOME"
grep -qE '\$HOME' <<<"$(grep -vE '^\s*#' "$repo/infra/forge/converge-hold.sh")" && fail "converge-hold.sh still reads \$HOME"
bash "$repo/infra/forge/converge-hold.sh" hold learning-the-new-runner >/dev/null || fail "hold failed"
[[ "$(<"$tmp/hold")" == "learning-the-new-runner" ]] || fail "the hold file does not carry the reason"
# shellcheck source=/dev/null
. "$repo/infra/forge/cluster-deploy-lib.sh"
reason=$(converge_held "$tmp/hold") || fail "the runner's hold check did not see the hold"
[[ "$reason" == "learning-the-new-runner" ]] || fail "the hold check returned '$reason'"
bash "$repo/infra/forge/converge-hold.sh" release >/dev/null || fail "release failed"
converge_held "$tmp/hold" >/dev/null && fail "a released hold still holds"
bash "$repo/infra/forge/converge-hold.sh" hold 2>/dev/null && fail "a hold with no reason was accepted"
# A hold that could not be written is NOT HELD, exit 1 (backlog d94d287e):
# it used to print HELD whatever its write had done, so an operator was
# told the converge was stopped when nothing stood in its way.
BOSS_CONVERGE_HOLD="$tmp/absent/hold" bash "$repo/infra/forge/converge-hold.sh" hold never-written >/dev/null 2>&1 \
    && fail "a hold whose write failed exited 0"
# install.sh runs prepare as root under systemd — no HOME there either.
env -i PATH="$PATH" BOSS_CONVERGE_HOLD="$tmp/state/hold" BOSS_CONVERGE_HOLD_LEGACY="$tmp/legacy" \
    bash "$repo/infra/forge/converge-hold.sh" prepare >/dev/null || fail "converge-hold.sh prepare needs HOME (install.sh runs it as root)"

# publish-github-pr: its --check validates inputs with no network, under
# the runner's environment (no HOME). It holds no token: since backlog
# d2b7c947 the verb's token is the GitHub App installation's, minted per
# request by the broker and rendered only by a run — so --check names the
# mint rule and the slot, and never a fork or a personal token file.
pub="$repo/infra/forge/publish-github-pr.sh"
env -i PATH="$PATH" bash -n "$pub" || fail "publish-github-pr.sh does not parse"
grep -qE '\$HOME' <<<"$(grep -vE '^\s*#' "$pub")" && fail "publish-github-pr.sh reads \$HOME (the ops runner has none)"
# Every spelling that turns xtrace on — `set -x`, `set -ex`, `set -euxo
# pipefail`, `set -o xtrace`, `set -uo xtrace` — not only the first
# (adversarial review of 78959555, L4).
XTRACE_ON='(^|[;&|({[:space:]])set[[:space:]]+(-[A-Za-z]*x[A-Za-z]*|-[A-Za-z]*o[[:space:]]+xtrace)([[:space:];]|$)'
grep -qE "$XTRACE_ON" <<<"$(grep -vE '^\s*#' "$pub")" && fail "publish-github-pr.sh traces (set -x) — a trace would print the token's environment"
# github-act.sh (the four GitHub verbs) runs under the same environment
# and carries a token: it must parse, need no HOME, and never trace. Its
# behaviour is boss-testing/tests/github_act_sh.rs.
gha="$repo/infra/forge/github-act.sh"
env -i PATH="$PATH" bash -n "$gha" || fail "github-act.sh does not parse"
grep -qE '\$HOME' <<<"$(grep -vE '^\s*#' "$gha")" && fail "github-act.sh reads \$HOME (the ops runner has none)"
grep -qE "$XTRACE_ON" <<<"$(grep -vE '^\s*#' "$gha")" && fail "github-act.sh traces (set -x) — a trace would print the token's header"
# And it turns an INHERITED xtrace off before anything else runs.
[ "$(sed -n '2p' "$gha" | cut -d'#' -f1 | tr -d '[:space:]')" = "set+x" ] \
    || fail "github-act.sh's first command (line 2) is not set +x — an inherited SHELLOPTS=xtrace would trace the token"
for bad in 'set -x' 'set -ex' 'set -euxo pipefail' 'set -o xtrace' 'set -uo xtrace' 'foo; set -x'; do
    grep -qE "$XTRACE_ON" <<<"$bad" || fail "the xtrace pattern misses '$bad'"
done
for ok in 'set +x' 'set -uo pipefail' 'set -euo pipefail' 'offset -x'; do
    grep -qE "$XTRACE_ON" <<<"$ok" && fail "the xtrace pattern refuses '$ok'"
done
mkdir -p "$tmp/bin" "$tmp/state" "$tmp/etc"
# --check only asks that gh and curl EXIST, so stubs stand in for them.
# jq it USES — it reads the publisher off the verb file (backlog
# d2b7c947) — so a real one is required, and a box without one (the
# forge and the gate image have it) does not exercise --check here.
for t in gh curl; do printf '#!/bin/sh\nexit 0\n' > "$tmp/bin/$t"; chmod +x "$tmp/bin/$t"; done
# The forge stand-in carries a `main` commit: since 2026-09-11 --check
# FETCHES refs/heads/main from it, because `-c safe.directory=<src>`
# cannot exempt a fetch SOURCE and a check that only READ the directory
# passed twice while the publish failed (ops-request c258d3b7).
git init -q --bare "$tmp/forge.git"
fixture_git() { env -i PATH="$PATH" GIT_AUTHOR_NAME=fixture GIT_AUTHOR_EMAIL=f@example.invalid \
    GIT_COMMITTER_NAME=fixture GIT_COMMITTER_EMAIL=f@example.invalid git -C "$tmp/forge.git" "$@"; }
seed_tree=$(fixture_git hash-object -t tree -w --stdin </dev/null) \
    || fail "could not write the fixture's empty tree"
seed_commit=$(fixture_git commit-tree "$seed_tree" -m seed) \
    || fail "could not write the fixture's seed commit"
fixture_git update-ref refs/heads/main "$seed_commit" \
    || fail "could not point the fixture's main at $seed_commit"
# The forge's address file (infra/lib/sor.sh): on the host the verb reads
# /etc/boss/sor.env for the forge's clone base; here the same file,
# rendered from the one source into the scratch root (backlog 5222163e).
bash "$repo/infra/estate/render-sor-env.sh" --to "$tmp/sor.env" >/dev/null \
    || fail "could not render the address file from infra/estate/estate.toml"
checkenv=(env -i PATH="$tmp/bin:$PATH" BOSS_PUBLISH_STATE_DIR="$tmp/state" BOSS_FORGE_REPO_PATH="$tmp/forge.git" BOSS_SOR_ENV="$tmp/sor.env")
check_line="publish-github-pr --check not exercised here (no jq on this box)"
if command -v jq >/dev/null 2>&1; then
    out=$("${checkenv[@]}" bash "$pub" --check 2>&1) \
        || fail "publish-github-pr.sh --check refused a complete input set: $out"
    grep -q -- '--check ok' <<<"$out" || fail "--check did not report ok: $out"
    grep -q 'broker-mints-the-algedonic-dev-publish-token-when-a-publish-request-is-filed' <<<"$out" \
        || fail "--check does not name the rule that mints the publish token: $out"
    grep -qi 'fork' <<<"$out" && fail "--check still names a fork — the publish opens its PR from the mirror itself (backlog d2b7c947): $out"
    grep -q 'github\.token' <<<"$out" && fail "--check still names the personal token file (backlog d2b7c947): $out"
    check_line="publish-github-pr --check passes on complete inputs, names the publish mint and no fork or personal token"
fi
# A run (no --check) with no system of record refuses before touching
# anything — the ops-runner rule, and the reason nothing here needs a
# network to prove. No address file for this one (the forge's clone URL
# named explicitly, so the refusal is the record's, not the forge's).
"${checkenv[@]}" BOSS_SOR_ENV="$tmp/absent.env" BOSS_FORGE_PUSH_URL="http://forge.test/david/boss.git" \
    bash "$pub" >/dev/null 2>&1 \
    && fail "a run without BOSS_JOBS_URL did not refuse"
# THROUGH THE RUNNER: the allowed literal is exercised the way a packet
# would — ops-runner.sh against a stubbed system of record (a GET serves
# one open packet, a PUT records the completion) and the real allowlist
# with its script path rewritten to this tree. `--check` must be
# ANSWERED (the verb's own check ran and said ok); a word outside the
# literal list must be REFUSED with the reason on the step, named,
# and identical to the journal line (6964f9e8: the reason lived only
# in the forge journal). The runner is sh + jq; this box may lack jq.
runner_line="runner path not exercised here (no jq on this box)"
if command -v jq >/dev/null 2>&1; then
    mkdir -p "$tmp/rbin" "$tmp/rstate"
    printf '#!/bin/sh\nexit 0\n' > "$tmp/rbin/gh"; chmod +x "$tmp/rbin/gh"
    cat > "$tmp/rbin/curl" <<'EOF'
#!/bin/sh
# The system of record, stubbed: every write answers 200 (the runner reads the status, 3c3b202c) and the step merge door's body - the keys the runner records on the step, sent before a status-only PUT (2aa2b19e) - is kept; a GET serves the fixture.
for a in "$@"; do case "$a" in @*)
    for u in "$@"; do case "$u" in */steps/*/metadata) cp "${a#@}" "$STUB_STEP_METADATA";; esac; done
    printf 200; exit 0;; esac; done
cat "$STUB_JOBS"
EOF
    chmod +x "$tmp/rbin/curl"
    # The allowlist is used VERBATIM — the directory of verb files copied
    # as-is: its scripts are repo-relative and the runner resolves them
    # against OPS_REPO_ROOT (66077f9c).
    mkdir -p "$tmp/verbs" && cp "$repo"/infra/ops/verbs/*.json "$tmp/verbs/"
    packet() { # $1 = args JSON array
        printf '{"data":[{"id":"aaaaaaaa-0000-4000-8000-000000000000","status":"open","metadata":{"host":"forge","verb":"publish-github-pr","args":%s},"steps":[{"id":"s-execute","spec_slug":"execute","status":"ready","metadata":{"authority_role":"platform-admin"}}]}]}' "$1" > "$tmp/jobs.json"
    }
    run_runner() {
        env -i PATH="$tmp/rbin:$PATH" HOST_ID=forge BOSS_JOBS_URL=http://sor.invalid \
            OPS_VERBS_DIR="$tmp/verbs" STUB_JOBS="$tmp/jobs.json" STUB_STEP_METADATA="$tmp/step-md.json" \
            BOSS_PUBLISH_STATE_DIR="$tmp/rstate" BOSS_FORGE_REPO_PATH="$tmp/forge.git" \
            BOSS_SOR_ENV="$tmp/sor.env" \
            sh "$repo/infra/ops/ops-runner.sh" 2>&1
    }
    rm -f "$tmp/step-md.json"; packet '["--check"]'
    out=$(run_runner) || fail "the runner failed on publish-github-pr --check: $out"
    [[ -f "$tmp/step-md.json" ]] || fail "the runner completed no step for --check: $out"
    [[ "$(jq -r .disposition "$tmp/step-md.json")" == answered ]] || fail "--check was not answered through the runner: $(cat "$tmp/step-md.json") / $out"
    grep -q -- '--check ok' <<<"$(jq -r .output "$tmp/step-md.json")" || fail "--check through the runner did not report ok: $(cat "$tmp/step-md.json")"
    rm -f "$tmp/step-md.json"; packet '["--force"]'
    out=$(run_runner) || fail "the runner failed refusing --force: $out"
    [[ "$(jq -r .disposition "$tmp/step-md.json")" == refused ]] || fail "--force was not refused: $(cat "$tmp/step-md.json")"
    reason=$(jq -r '.reason // empty' "$tmp/step-md.json")
    [[ -n "$reason" ]] || fail "the refusal wrote no reason on the step: $(cat "$tmp/step-md.json")"
    [[ "$reason" == *"not one of --check"* ]] || fail "the reason does not name the literal list: $reason"
    [[ "$reason" == "$(jq -r .output "$tmp/step-md.json")" ]] || fail "reason and output differ on a refusal"
    grep -qF -- "refused aaaaaaaa — $reason" <<<"$out" || fail "the journal line does not carry the same reason: $out"
    runner_line="through the runner, publish-github-pr --check is answered and --force is refused with the reason on the step"
fi
# delete-orphan-object: the bounds that need no cluster, exercised. The
# derivation's own behaviour against a stubbed kubectl is
# boss-testing/tests/delete_orphan_object_sh.rs; here we pin only that
# the argument bound refuses BEFORE anything looks at a cluster, under
# the ops-runner's environment (no HOME).
dob="$repo/infra/forge/delete-orphan-object.sh"
der="$repo/infra/cluster/undeclared-objects.sh"
env -i PATH="$PATH" bash -n "$dob" || fail "delete-orphan-object.sh does not parse"
env -i PATH="$PATH" bash -n "$der" || fail "undeclared-objects.sh does not parse"
grep -qE '\$HOME' <<<"$(grep -vE '^\s*#' "$dob")" && fail "delete-orphan-object.sh reads \$HOME (the ops runner has none)"
grep -qE '\$HOME' <<<"$(grep -vE '^\s*#' "$der")" && fail "undeclared-objects.sh reads \$HOME (the ops runner has none)"
# EVERY git CALL IN THE VERB GOES THROUGH THE OWNER.
#
# The ops-runner executes verbs as root and the forge checkout belongs to a
# user, and git refuses to READ across that boundary ("dubious ownership",
# 2.35.2+) as firmly as a root WRITE would leave root-owned objects behind.
# A bare `git -C "$TREE"` is therefore a command that fails on every real
# invocation while passing every test here, because a fixture is owned by
# whoever runs the gate — measured, on ops-request c9877f75, 2026-09-10.
# Structural, for the same reason boss-gcp-converges-itself.sh §2b is: the
# runuser branch cannot be exercised without a second account.
bare=$(grep -nE '(^|[^_"])git -C "\$TREE"' "$dob" || true)
[ -z "$bare" ] || fail "delete-orphan-object.sh calls git outside as_owner:
$bare
    Root cannot even READ a checkout it does not own; wrap it:
      as_owner \"git -C '\$TREE' <args>\""
grep -q 'as_owner()' "$dob" || fail "delete-orphan-object.sh has no as_owner — its git reads cannot be running as the checkout's owner"
grep -q 'stat -c %U' "$dob" || fail "delete-orphan-object.sh does not read the owner off the directory (hardcoding an account silently corrupts a host that moves the checkout)"
grep -q 'UNKNOWN' "$dob" || fail "delete-orphan-object.sh does not handle stat's UNKNOWN (no passwd entry for the owning uid), so it would fall back to reading git as the caller"
# And the derivation needs none of this, which is only true while it makes
# no git call and writes nothing under the checkout. Pin both, because the
# day either changes it acquires the same defect silently.
der_git=$(grep -vE '^\s*#' "$der" | grep -nE '(^|[^-[:alnum:]_.])git[[:space:]]' || true)
[ -z "$der_git" ] || fail "undeclared-objects.sh now calls git:
$der_git
    It runs as root under the ops-runner against a user-owned checkout, so a
    git call there needs the same as_owner drop delete-orphan-object.sh uses."
der_writes=$(grep -nE '>[[:space:]]*"?\$(TREE|DIR)' "$der" || true)
[ -z "$der_writes" ] || fail "undeclared-objects.sh writes under the checkout:
$der_writes
    Root writing in a user-owned clone is what breaks the owner's later pulls.
    Keep its scratch in mktemp."

for bad in "boss-docs-internal" "service/boss/x" "Service/boss"; do
    out=$(env -i PATH="$PATH" bash "$dob" "$bad" 2>&1) \
        && fail "delete-orphan-object.sh accepted the malformed target '$bad'"
    grep -q '<Kind>/<namespace>/<name>' <<<"$out" || fail "the refusal of '$bad' does not name the shape: $out"
done
out=$(env -i PATH="$PATH" bash "$dob" "Service/boss/x" --force 2>&1) \
    && fail "delete-orphan-object.sh accepted a second argument other than --dry-run"
grep -q -- '--dry-run' <<<"$out" || fail "the refusal does not name the only allowed mode: $out"

echo "the-controls-are-bounded-verbs: self-test ok — every MUTATING ops verb is bounded and authorized; the hold round-trips through the file the runner reads, with no HOME in the environment; a hold needs a reason; a rollback needs a sha; $check_line, and a run refuses without a system of record; $runner_line"
exit 0
