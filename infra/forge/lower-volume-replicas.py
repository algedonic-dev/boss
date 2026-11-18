"""Signed one-copy reduction; maintenance tickets never substitute for RW proof."""
import datetime as dt
import ctypes
import hashlib
import json
import os
from pathlib import Path
import re
import select
import signal
import subprocess
import sys
import tempfile
import time

HERE = Path(__file__).resolve().parent
LH = "longhorn-system"
WINDOW = 1800  # covers the registered operation's entire bounded lifetime
WAIT = 180
OPERATION_SECONDS = 1500
CLEANUP_SECONDS = 300
FENCE_ANNOTATION = "boss.algedonic.dev/lower-replica-fence"
SOURCE_FILES = (
    "infra/forge/retire-volume-replica.sh", "infra/forge/lower-volume-replicas.py",
    "infra/forge/lower-volume-replicas.sh", "infra/lib/sor.sh", "infra/lib/sor-reader.sh", "infra/lib/jq.sh",
    "infra/lib/longhorn-ledger.sh", "infra/estate/ops-credentials.sh")


class Refusal(Exception):
    pass


class Failure(Refusal):
    pass


def require(ok, why):
    if not ok:
        raise Refusal(why)


def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False)


def digest(value):
    return hashlib.sha256(canonical(value).encode()).hexdigest()


class Budget:
    def __init__(self, seconds=None, end=None):
        self.end = end if end is not None else time.monotonic() + seconds

    def remaining(self, cap):
        left = self.end - time.monotonic()
        require(left > 0, "absolute operation deadline exhausted")
        return min(cap, left)


def supervised(work, cleanup, budget, cleanup_seconds=CLEANUP_SECONDS):
    """One bounded owner survives the runner group; it never outlives its deadline.

    Only the work child uses the destructive engine. The owner stops and reaps
    owned host descendants; that cannot cancel requests in remote containers.
    Production cleanup fences the attachment before exact own-ticket removal.
    EOF witnesses launcher death, including SIGKILL; no finally in that launcher
    is needed. Killing this owner or losing the host still leaves explicit debt.
    """
    require(budget.end - time.monotonic() > cleanup_seconds + 5,
            "no work admitted without cleanup and termination headroom")
    alive_r, alive_w = os.pipe()
    owner = os.fork()
    if owner:
        os.close(alive_r)
        try:
            _, status = os.waitpid(owner, 0)
        finally:
            os.close(alive_w)
        if os.waitstatus_to_exitcode(status) != 0:
            raise Failure("operation stopped or failed; cleanup owner diagnostic retained, effect UNKNOWN")
        return
    os.close(alive_w)
    code = 1
    try:
        os.setsid()
        # Linux forge: adopted engine grandchildren belong to this operation,
        # not PID 1. No other case runs in this fixed owner process.
        require(sys.platform == "linux" and ctypes.CDLL(None, use_errno=True).prctl(36, 1, 0, 0, 0) == 0,
                "Linux child reaper unavailable; nothing written")
        worker = os.fork()
        if worker == 0:
            os.close(alive_r)
            os.setpgid(0, 0)
            try:
                work()
                sys.stdout.flush(); sys.stderr.flush()
                os._exit(0)
            except BaseException as exc:
                print(f"lower-volume-replicas work failed: {exc}", file=sys.stderr, flush=True)
                sys.stdout.flush(); sys.stderr.flush()
                os._exit(1)
        # Close the setpgid race before any launcher-death cleanup.
        try:
            os.setpgid(worker, worker)
        except (PermissionError, ProcessLookupError):
            pass
        work_end = budget.end - cleanup_seconds - 5
        status = None
        interrupted = False
        while status is None:
            pid, value = os.waitpid(worker, os.WNOHANG)
            if pid:
                status = value
                break
            readable, _, _ = select.select([alive_r], [], [], min(0.1, max(0, work_end - time.monotonic())))
            if time.monotonic() >= work_end or (readable and not os.read(alive_r, 1)):
                interrupted = True
                break
        # Stop owned host descendants even after normal/failed leader exit.
        # Remote writes can survive their clients: cleanup must fence them,
        # and an unknown count/delete outcome remains UNKNOWN after a snapshot.
        if status is None:
            # The unreaped leader still owns this numeric identity. Once
            # waitpid reaps it, signal only our adopted children below.
            try:
                os.killpg(worker, signal.SIGKILL)
            except ProcessLookupError:
                pass
            _, status = os.waitpid(worker, 0)
        reap_end = min(budget.end, time.monotonic() + 5)
        while True:
            try:
                pid, _ = os.waitpid(-1, os.WNOHANG)
            except ChildProcessError:
                break
            if pid:
                continue
            # GNU timeout and a transport may create another process group.
            # Subreaping, not a PGID alone, identifies all of our remaining
            # descendants; no unrelated process is reaped or signalled here.
            children = Path(f"/proc/self/task/{os.getpid()}/children").read_text().split()
            for child in children:
                try:
                    os.kill(int(child), signal.SIGKILL)
                except ProcessLookupError:
                    pass
            require(time.monotonic() < reap_end,
                    "owned child did not terminate; cleanup debt, ticket retained")
            time.sleep(0.01)
        cleanup()
        code = 0 if not interrupted and os.waitstatus_to_exitcode(status) == 0 else 1
        if code:
            print("lower-volume-replicas: work stopped; effect UNKNOWN/partial; owned cleanup returned", file=sys.stderr, flush=True)
    except BaseException as exc:
        print(f"lower-volume-replicas: cleanup debt or owner refusal: {exc}", file=sys.stderr, flush=True)
    finally:
        os.close(alive_r)
    sys.stdout.flush(); sys.stderr.flush()
    os._exit(code)


def wall_now():
    # The same explicit UTC clock surface as the shared retirement engine;
    # owning tests supply its deterministic clock, never a fabricated plan time.
    p = subprocess.run(["date", "-u", "+%s"], capture_output=True, text=True, timeout=5)
    require(p.returncode == 0 and p.stdout.strip().isdigit(), "UTC clock UNKNOWN")
    return int(p.stdout.strip())


class Door:
    def __init__(self, argv, budget=None):
        self.argv = argv
        self.budget = budget or Budget(OPERATION_SECONDS)

    def call(self, *args, body=None):
        p = subprocess.run(self.argv + list(args) + ["--request-timeout=20s"],
                           input=body, capture_output=True, text=True,
                           timeout=self.budget.remaining(25))
        if p.returncode:
            # Retain the actual refusal, not a guessed write effect.
            print(p.stdout, end="", file=sys.stderr)
            print(p.stderr, end="", file=sys.stderr)
            raise Failure(f"kubectl {args[0]} failed ({p.returncode}); effect UNKNOWN")
        return p.stdout

    def get(self, resource, name=None, namespace=LH):
        args = ["get", resource] + ([name] if name else [])
        if namespace:
            args += ["-n", namespace]
        try:
            obj = json.loads(self.call(*args, "-o", "json"))
        except ValueError as exc:
            raise Failure("unread Kubernetes JSON") from exc
        require(isinstance(obj, dict), "unread typed Kubernetes response")
        kind = {"volumes.longhorn.io": "Volume", "replicas.longhorn.io": "Replica",
                "nodes.longhorn.io": "Node", "engines.longhorn.io": "Engine",
                "engineimages.longhorn.io": "EngineImage", "volumeattachments.longhorn.io": "VolumeAttachment",
                "volumeattachments.storage.k8s.io": "VolumeAttachment", "pvc": "PersistentVolumeClaim",
                "pods": "Pod", "jobs": "Job", "cronjobs": "CronJob"}[resource]
        if name:
            require(obj.get("kind") == kind and obj.get("metadata", {}).get("name") == name and
                    obj.get("metadata", {}).get("namespace") == namespace and
                    bool(obj["metadata"].get("uid")), "object identity/UID UNKNOWN")
        else:
            items = obj.get("items")
            require(obj.get("kind") in ("List", kind + "List") and isinstance(items, list)
                    and not obj.get("metadata", {}).get("continue")
                    and not obj.get("metadata", {}).get("remainingItemCount"),
                    "incomplete/paginated inventory")
            names = [x.get("metadata", {}).get("name") for x in items]
            require(all(names) and len(names) == len(set(names)), "ambiguous inventory")
            require(all(x.get("kind") == kind and x.get("metadata", {}).get("uid") and
                        (namespace is None or x["metadata"].get("namespace") == namespace)
                        for x in items), "inventory kind/namespace/UID UNKNOWN")
        return obj

    def patch(self, resource, obj, patch):
        meta = obj["metadata"]
        fence = [{"op": "test", "path": "/metadata/uid", "value": meta["uid"]},
                 {"op": "test", "path": "/metadata/resourceVersion", "value": meta["resourceVersion"]}]
        raw = self.call("patch", resource, meta["name"], "-n", LH, "--type=json",
                        "-p", canonical(fence + patch))
        print(raw, end="", flush=True)


def identity(replica):
    m, s = replica["metadata"], replica["spec"]
    require(m.get("uid") and s.get("nodeID") and s.get("diskID"), "replica identity UNKNOWN")
    return {"name": m["name"], "uid": m["uid"], "node": s["nodeID"], "disk": s["diskID"]}


def live_replica(r):
    s = r.get("spec", {})
    return (bool(s.get("healthyAt")) and not s.get("failedAt") and s.get("active") is True
            and not r["metadata"].get("deletionTimestamp") and r.get("status", {}).get("currentState") == "running")


def inventory(door, volume):
    v = door.get("volumes.longhorn.io", volume)
    reps = door.get("replicas.longhorn.io")["items"]
    mine = sorted((x for x in reps if x.get("spec", {}).get("volumeName") == volume),
                  key=lambda x: x["metadata"]["name"])
    require(mine, "replica inventory empty")
    return v, mine


def claim_idle(door, v, now):
    k = v["status"].get("kubernetesStatus", {})
    ns, claim, pv = k.get("namespace"), k.get("pvcName"), k.get("pvName")
    require(ns and claim and pv == v["metadata"]["name"], "claim/PV identity UNKNOWN")
    pvc = door.get("pvc", claim, ns)
    require(pvc.get("status", {}).get("phase") == "Bound" and
            pvc.get("spec", {}).get("volumeName") == pv and not pvc["metadata"].get("deletionTimestamp"),
            "claim binding changed")
    def uses(spec):
        return any(x.get("persistentVolumeClaim", {}).get("claimName") == claim for x in spec.get("volumes", []))
    for x in door.get("pods", namespace=ns)["items"]:
        require(not uses(x.get("spec", {})) or x.get("status", {}).get("phase") in ("Succeeded", "Failed"),
                "live/Pending Pod holds detached claim")
    for x in door.get("jobs", namespace=ns)["items"]:
        terminal = any(c.get("type") in ("Complete", "Failed") and c.get("status") == "True"
                       for c in x.get("status", {}).get("conditions", []))
        require(not uses(x.get("spec", {}).get("template", {}).get("spec", {})) or
                (terminal and not x.get("status", {}).get("active")), "unfinished Job holds detached claim")
    for x in door.get("volumeattachments.storage.k8s.io", namespace=None)["items"]:
        require(x.get("spec", {}).get("source", {}).get("persistentVolumeName") != pv,
                "Kubernetes VolumeAttachment holds detached claim")
    schedules = []
    for x in door.get("cronjobs", namespace=ns)["items"]:
        s = x["spec"]
        if not uses(s.get("jobTemplate", {}).get("spec", {}).get("template", {}).get("spec", {})):
            continue
        require(s.get("timeZone") == "Etc/UTC", "relevant CronJob timeZone UNKNOWN; require explicit Etc/UTC")
        fields = s.get("schedule", "").split()
        require(len(fields) == 5 and fields[2:] == ["*", "*", "*"] and
                fields[0].isdigit() and fields[1].isdigit(), "unsupported/unknown CronJob schedule")
        minute, hour = map(int, fields[:2])
        require(0 <= minute < 60 and 0 <= hour < 24, "invalid CronJob schedule")
        daily = hour * 3600 + minute * 60
        distance = ((int(now) % 86400) - daily) % 86400
        require(WINDOW < distance < 86400 - WINDOW, "near scheduled claim use; maintenance refused")
        schedules.append({"name": x["metadata"]["name"], "uid": x["metadata"]["uid"],
                          "schedule": s["schedule"], "timeZone": s["timeZone"]})
    return {"namespace": ns, "claim": claim, "claim_uid": pvc["metadata"]["uid"],
            "pv": pv, "schedules": sorted(schedules, key=lambda x: x["name"]),
            "exclusion_seconds": WINDOW}


def attachment(door, context):
    va = door.get("volumeattachments.longhorn.io", context["volume"])
    require(va.get("spec", {}).get("volume") == context["volume"],
            "attachment references another or unknown volume")
    volume = door.get("volumes.longhorn.io", context["volume"])
    require(volume["metadata"]["uid"] == context["volume_uid"], "referenced volume UID changed")
    require("va_uid" not in context or va["metadata"]["uid"] == context["va_uid"],
            "attachment UID changed")
    return va


def own_ticket(va, context):
    tickets = va["spec"].get("attachmentTickets", {})
    require(isinstance(tickets, dict), "attachment tickets unread; cleanup debt")
    owned = tickets.get(context["ticket_id"])
    require(owned is None or owned == context["ticket"], "owned ticket changed; cleanup debt")
    # A consumer may arrive while maintenance runs. Its ticket must survive,
    # but retaining our frontend-disabled ticket would prevent its frontend.
    # Destructive work separately refuses any foreign ticket in runtime().
    return owned


def attachment_version(va):
    version = va["metadata"].get("resourceVersion")
    require(isinstance(version, str) and version, "attachment version UNKNOWN; cleanup debt")
    return version


def reconcile_ticket(door, context, budget):
    # This unconditional fence also covers a killed work child whose attach
    # submission/ACK never reached its parent. A client PID is not a fence.
    while True:
        budget.remaining(25)
        before = attachment(door, context)
        own_ticket(before, context)
        version = attachment_version(before)
        annotations = before["metadata"].get("annotations", {})
        require(isinstance(annotations, dict), "attachment annotations unread; cleanup debt")
        old = annotations.get(FENCE_ANNOTATION)
        first = context["ticket_id"] + ":0"
        value = context["ticket_id"] + ":1" if old == first else first
        require(value != old, "attachment fence would be a no-op; cleanup debt")
        if "annotations" not in before["metadata"]:
            change = {"op": "add", "path": "/metadata/annotations", "value": {FENCE_ANNOTATION: value}}
        else:
            key = FENCE_ANNOTATION.replace("~", "~0").replace("/", "~1")
            change = {"op": "add", "path": "/metadata/annotations/" + key, "value": value}
        try:
            door.patch("volumeattachments.longhorn.io", before, [change])
        except (Failure, subprocess.SubprocessError) as error:
            print(f"lower-volume-replicas: fence ACK UNKNOWN; rereading: {error}", file=sys.stderr, flush=True)
        after = attachment(door, context)
        owned = own_ticket(after, context)
        require(attachment_version(after) != version,
                "attachment fence did not prove changed resourceVersion; cleanup debt")
        if after["metadata"].get("annotations", {}).get(FENCE_ANNOTATION) != value:
            # A late attach/other CAS won first. Reread and fence that version;
            # the opaque resourceVersion is compared for equality, never order.
            time.sleep(budget.remaining(0.05))
            continue
        if owned is None:
            return
        remove_error = None
        try:
            door.patch("volumeattachments.longhorn.io", after, [
                {"op": "test", "path": "/spec/attachmentTickets/" + context["ticket_id"], "value": context["ticket"]},
                {"op": "remove", "path": "/spec/attachmentTickets/" + context["ticket_id"]}])
        except (Failure, subprocess.SubprocessError) as error:
            remove_error = error
            print(f"lower-volume-replicas: remove ACK UNKNOWN; rereading: {error}", file=sys.stderr, flush=True)
        final = attachment(door, context)
        remaining = own_ticket(final, context)
        require(attachment_version(final) != attachment_version(after),
                "own-ticket removal not observed; cleanup debt")
        if remaining is None:
            return
        require(remove_error is not None, "own ticket remains after acknowledged removal; cleanup debt")
        # A conflicting write changed the version while preserving our exact
        # ticket. A new bounded fence/read/remove attempts only that ticket.


def final_observation(door, context):
    try:
        volume, replicas = inventory(door, context["volume"])
        require(volume["metadata"]["uid"] == context["volume_uid"], "final volume UID changed")
        count = volume["spec"].get("numberOfReplicas")
        require(type(count) is int and count >= 0, "final declared count UNKNOWN")
        result = {"state": "observed", "volume_uid": volume["metadata"]["uid"],
                  "declared_count": count, "replicas": [identity(replica) for replica in replicas],
                  "meaning": "final snapshot only; stopped or unknown writes remain UNKNOWN/partial and may land later"}
    except (Refusal, ValueError, KeyError, TypeError, subprocess.SubprocessError) as error:
        result = {"state": "UNKNOWN", "volume_uid": context["volume_uid"],
                  "declared_count": None, "replicas": None, "error": str(error)}
        print("lower-volume-replicas final-observation: " + canonical(result), flush=True)
        raise Failure("final observation unread; effect UNKNOWN") from error
    print("lower-volume-replicas final-observation: " + canonical(result), flush=True)


def runtime(door, context, phase):
    v, reps = inventory(door, context["volume"])
    require(v["metadata"]["uid"] == context["volume_uid"], "volume UID changed")
    require(v["spec"].get("numberOfReplicas") == (context["count"] if phase == "before-lower" else context["target"]),
            "current declared count changed")
    expected = context["replicas"]
    if phase == "effect":
        expected = [x for x in expected if x["name"] != context["replica"]]
    require([identity(x) for x in reps] == expected, "signed replica set/UID/placement changed")
    require(len({x["spec"]["nodeID"] for x in reps}) == len(reps) and all(live_replica(x) for x in reps),
            "fresh running healthy distinct replicas not proven")
    nodes = door.get("nodes.longhorn.io")["items"]
    for r in reps:
        selected = [n for n in nodes if n["metadata"]["name"] == r["spec"]["nodeID"]]
        require(len(selected) == 1, "replica node UNKNOWN")
        n = selected[0]
        require(any(x.get("type") == "Ready" and x.get("status") == "True"
                    for x in n.get("status", {}).get("conditions", [])), "replica node not Ready")
        disks = [d for d in n.get("status", {}).get("diskStatus", {}).values()
                 if d.get("diskUUID") == r["spec"]["diskID"]]
        require(len(disks) == 1 and any(x.get("type") == "Ready" and x.get("status") == "True"
                    for x in disks[0].get("conditions", [])), "replica disk not Ready")
    require(v["status"].get("state") == "attached" and v["status"].get("robustness") == "healthy"
            and v["status"].get("currentNodeID") == context["attach_node"]
            and v["spec"].get("nodeID") == context["attach_node"]
            and not v["spec"].get("migrationNodeID") and not v["status"].get("restoreRequired"),
            "attached current identity/health UNKNOWN")
    engines = [e for e in door.get("engines.longhorn.io")["items"]
               if e.get("spec", {}).get("volumeName") == context["volume"]
               and not e["metadata"].get("deletionTimestamp")]
    require(len(engines) == 1, "current engine ambiguous")
    e = engines[0]
    names = sorted(x["name"] for x in expected)
    modes = e.get("status", {}).get("replicaModeMap", {})
    require(e["spec"].get("nodeID") == context["attach_node"] and
            e.get("status", {}).get("currentState") == "running" and
            e["spec"].get("engineImage", e["spec"].get("image")) == context["image"] and
            sorted(e["spec"].get("replicaAddressMap", {})) == names and
            sorted(modes) == names and all(modes[n] == "RW" for n in names),
            "current engine exact RW roster UNKNOWN")
    for field, active in (("restoreStatus", "isRestoring"), ("purgeStatus", "isPurging")):
        statuses = e.get("status", {}).get(field, {})
        require(isinstance(statuses, dict) and all(isinstance(x, dict) and not x.get(active)
                and not x.get("error") for x in statuses.values()), "engine restore/purge is active or failed")
    require(not v["spec"].get("salvageRequested") and not v["status"].get("isStandby") and
            not any(x.get("type") == "Restore" and x.get("status") == "True"
                    for x in v["status"].get("conditions", [])), "restore/salvage state is not admitted")
    if context["maintenance"]:
        require(claim_idle(door, v, wall_now()) == context["claim"], "claim/schedule changed")
        va = attachment(door, context)
        tickets = va["spec"].get("attachmentTickets", {})
        require(tickets == {context["ticket_id"]: context["ticket"]}, "foreign or changed attachment ticket")
        status = va.get("status", {}).get("attachmentTicketStatuses", {}).get(context["ticket_id"], {})
        require(status.get("satisfied") is True and status.get("generation") == context["ticket"]["generation"],
                "attachment ticket not satisfied for this generation")
    return v


def make_plan(door, volume, replica):
    v, reps = inventory(door, volume)
    n = v["spec"].get("numberOfReplicas")
    require(isinstance(n, int) and n >= 3 and len(reps) == n, "lowering must retain floor 2 and exact declared set")
    roster = [identity(x) for x in reps]
    require(any(x["name"] == replica for x in roster) and len({x["node"] for x in roster}) == n,
            "named replica/distinct nodes not proven")
    require(not v["metadata"].get("deletionTimestamp"), "volume deleting")
    maintenance = v["status"].get("state") == "detached"
    require(maintenance or v["status"].get("state") == "attached", "volume state not admitted")
    node = (next(x["node"] for x in roster if x["name"] != replica) if maintenance
            else v["status"].get("currentNodeID"))
    require(node, "attachment node UNKNOWN")
    c = {"version": 1, "volume": volume, "volume_uid": v["metadata"]["uid"], "replica": replica,
         "replicas": roster, "count": n, "target": n - 1, "floor": 2,
         "maintenance": maintenance, "attach_node": node, "image": v["spec"].get("image"),
         "bounded_seconds": OPERATION_SECONDS}
    c["source_sha256"] = {name: hashlib.sha256((HERE.parent.parent / name).read_bytes()).hexdigest()
                          for name in SOURCE_FILES}
    c["actions"] = ["optional owned frontend-disabled maintenance ticket; wait fresh exact RW set",
                    "shared engine signed recheck, UID/count CAS, settle and UID-fenced named delete",
                    "fresh exact retained RW readback; changed owned metadata RV fence; reread/remove only own ticket; detached/stopped snapshot"]
    require(isinstance(c["image"], str) and bool(c["image"]), "engine image UNKNOWN")
    if maintenance:
        require(not v["spec"].get("nodeID") and not v["status"].get("currentNodeID") and
                not v["spec"].get("migrationNodeID") and not v["status"].get("restoreRequired"),
                "volume not fully detached or restore pending")
        require(all(r.get("spec", {}).get("healthyAt") and not r["spec"].get("failedAt")
                    and r["spec"].get("active") is True and not r["metadata"].get("deletionTimestamp")
                    and r.get("status", {}).get("currentState") == "stopped" for r in reps),
                "offline replica eligibility UNKNOWN; historical healthyAt is not current health")
        engines = [e for e in door.get("engines.longhorn.io")["items"]
                   if e.get("spec", {}).get("volumeName") == volume]
        require(engines and all(e.get("status", {}).get("currentState") == "stopped" for e in engines),
                "fully stopped engine state UNKNOWN")
        c["claim"] = claim_idle(door, v, wall_now())
        va = attachment(door, c)
        require(not va["spec"].get("attachmentTickets"), "foreign attachment tickets")
        c["va_uid"] = va["metadata"]["uid"]
        nodes = door.get("nodes.longhorn.io")["items"]
        wanted = [x for x in nodes if x["metadata"]["name"] == node]
        require(len(wanted) == 1 and any(q.get("type") == "Ready" and q.get("status") == "True"
                for q in wanted[0].get("status", {}).get("conditions", [])), "maintenance node not Ready")
        images = [x for x in door.get("engineimages.longhorn.io")["items"]
                  if x.get("spec", {}).get("image") == c["image"]]
        require(len(images) == 1 and images[0].get("status", {}).get("state") == "deployed" and
                all(images[0]["status"].get("nodeDeploymentMap", {}).get(x["node"]) is True for x in roster),
                "engine image deployment UNKNOWN")
        c["ticket_id"] = "boss-lower-" + digest(c)[:24]
        c["ticket"] = {"id": c["ticket_id"], "type": "longhorn-api", "nodeID": node, "generation": 1,
                       "parameters": {"disableFrontend": "true", "lastAttachedBy": "boss-lower-volume-replicas"}}
    else:
        runtime(door, c, "before-lower")
    return c


def wait_until(check, budget):
    end = min(time.monotonic() + WAIT, budget.end)
    while True:
        try:
            return check()
        except Refusal as error:
            if time.monotonic() >= end:
                raise Refusal(f"bounded wait failed: {error}") from error
            time.sleep(min(5, max(0, end - time.monotonic())))


def main():
    budget = Budget(OPERATION_SECONDS)
    if sys.argv[1:2] == ["--started-at"]:
        started = float(sys.argv[2])
        require(0 <= started <= time.monotonic(), "operation start clock UNKNOWN")
        budget = Budget(end=started + OPERATION_SECONDS)
        del sys.argv[1:3]
    effect_result = None
    if sys.argv[1:2] == ["--effect-result"]:
        effect_result = Path(sys.argv[2])
        del sys.argv[1:3]
    if sys.argv[1:2] == ["--delete"]:
        c = json.loads(Path(sys.argv[2]).read_text())
        door = Door(c.pop("kubectl"), Budget(end=c.pop("deadline")))
        runtime(door, c, "before-delete")
        uid = next(x["uid"] for x in c["replicas"] if x["name"] == c["replica"])
        body = canonical({"apiVersion": "v1", "kind": "DeleteOptions",
                          "preconditions": {"uid": uid}, "gracePeriodSeconds": 30})
        print(door.call("delete", "--raw", f"/apis/longhorn.io/v1beta2/namespaces/{LH}/replicas/{c['replica']}",
                        "-f", "-", body=body), end="")
        return
    if sys.argv[1:2] == ["--guard"]:
        c = json.loads(Path(sys.argv[2]).read_text())
        runtime(Door(c.pop("kubectl"), Budget(end=c.pop("deadline"))), c, sys.argv[3])
        return
    split = sys.argv.index("--")
    door = Door(sys.argv[2:split], budget)
    args = sys.argv[split + 1:]
    plan_only = args[:1] == ["--plan"]
    if plan_only:
        args = args[1:]
    else:
        require(effect_result is not None, "registered shell final-result destination missing; nothing written")
    require(len(args) == (2 if plan_only else 3), "usage: [--plan] volume replica [signed_sha256]")
    volume, replica = args[:2]
    require(re.fullmatch(r"pvc-[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}", volume) and
            re.fullmatch(re.escape(volume) + r"-r-[0-9a-f]{8}", replica), "invalid volume/replica")
    c = make_plan(door, volume, replica)
    rendered = "plan: lower-volume-replicas\nnumberOfReplicas: %s -> %s — floor is 2\n%s\n" % (c["count"], c["target"], canonical(c))
    sha = hashlib.sha256(rendered.encode()).hexdigest()
    if plan_only:
        print(rendered, end="")
        print("plan-sha256: " + sha, file=sys.stderr)
        return
    require(args[2] == sha, "signed plan differs from current exact replica set; nothing written")
    print(rendered, end="", flush=True)
    work_budget = Budget(end=budget.end - CLEANUP_SECONDS - 5)

    def work():
        door.budget = work_budget
        if c["maintenance"]:
            va = attachment(door, c)
            require(va["metadata"]["uid"] == c["va_uid"] and not va["spec"].get("attachmentTickets"), "attachment changed")
            door.patch("volumeattachments.longhorn.io", va, [{"op": "add", "path": "/spec/attachmentTickets", "value": {c["ticket_id"]: c["ticket"]}}])
            wait_until(lambda: runtime(door, c, "before-lower"), work_budget)
        with tempfile.TemporaryDirectory(prefix="boss-lower-") as tmp:
            context = Path(tmp) / "context.json"
            context.write_text(canonical(dict(c, kubectl=door.argv, deadline=work_budget.end)))
            env = dict(os.environ, BOSS_RETIRE_FLOOR="2")
            engine = ["bash", str(HERE / "retire-volume-replica.sh"), "--lower-checked", str(context)]
            p = subprocess.run(engine + ["--plan", volume, replica], env=env, capture_output=True, text=True, timeout=work_budget.remaining(300))
            print(p.stdout, end="", flush=True); print(p.stderr, end="", file=sys.stderr, flush=True)
            require(p.returncode == 0, "shared retirement engine refused its current guard plan")
            hashes = re.findall(r"^plan-sha256: ([0-9a-f]{64})$", p.stderr, re.M)
            require(len(hashes) == 1, "shared engine plan hash UNKNOWN")
            p = subprocess.run(engine + [volume, replica, hashes[0]], env=env, timeout=work_budget.remaining(600))
            require(p.returncode == 0, "shared retirement engine failed; partial effect retained, no guessed rollback")
            runtime(door, c, "effect")
    def cleanup():
        door.budget = budget
        try:
            if c["maintenance"]:
                reconcile_ticket(door, c, budget)
                def detached():
                    v, reps = inventory(door, volume)
                    require(v["metadata"]["uid"] == c["volume_uid"] and v["status"].get("state") == "detached" and
                            not v["status"].get("currentNodeID") and not v["spec"].get("nodeID") and
                            all(x.get("status", {}).get("currentState") == "stopped" for x in reps), "detached/stopped cleanup not proven")
                    engines = door.get("engines.longhorn.io")["items"]
                    require(all(x.get("status", {}).get("currentState") == "stopped" for x in engines
                                if x.get("spec", {}).get("volumeName") == volume), "engine still running")
                wait_until(detached, budget)
        except BaseException as error:
            print(f"lower-volume-replicas: owned cleanup incomplete: {error}", file=sys.stderr, flush=True)
            raise
        finally:
            # Also runs for attached (maintenance=false) failures. A snapshot
            # conserves the actual partial count/UID set, never a settled effect.
            final_observation(door, c)
    supervised(work, cleanup, budget)
    require(effect_result is not None, "registered shell final-result destination missing; effect UNKNOWN")
    effect_result.write_text(canonical({"schema": 1, "volume": volume, "replica": replica,
                                        "target": c["target"], "maintenance": c["maintenance"]}))


if __name__ == "__main__":
    try:
        main()
    except (Refusal, ValueError, KeyError, TypeError, subprocess.SubprocessError) as exc:
        print(f"lower-volume-replicas: REFUSED/FAILED — {exc}", file=sys.stderr)
        sys.exit(1 if isinstance(exc, (Failure, subprocess.SubprocessError)) else 78)
