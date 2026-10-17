#!/usr/bin/python3 -I
"""Existing forge operations as explicit, fixed fixture ports.

The receiver never imports this module. Only the protected forge executor
can resolve native records, conserve originals or address Kubernetes.
"""
import base64
import importlib.util
import json
from pathlib import Path
import time

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("fixture_port_kernel", HERE / "isolation-fixture.py")
kernel = importlib.util.module_from_spec(spec)
spec.loader.exec_module(kernel)


class NativePort:
    def __init__(self, run, plan, prerequisites):
        self.run, self.plan, self.check = run, plan, prerequisites
        self.name = kernel.intents(plan)[0]["metadata"]["name"]

    def target(self, kind, name):
        if kind not in ("Job", "Service") or name != self.name:
            raise kernel.Unknown("native operation is outside the original fixed fixture")

    def prerequisites(self):
        return self.check()

    def get(self, kind, name):
        self.target(kind, name)
        raw = self.run(["get", kind, name, "-n", "boss-dev", "--ignore-not-found=true", "-o", "json", "--request-timeout=10s"])
        if raw == b"":
            return None  # Only a successful ignore-not-found read proves absence.
        value = json.loads(raw)
        if value.get("kind") != kind or value.get("metadata", {}).get("name") != name or value["metadata"].get("namespace") != "boss-dev":
            raise kernel.Unknown("native read returned another object")
        return value

    def create(self, body):
        if body not in kernel.intents(self.plan):
            raise kernel.Unknown("native create is not the complete fixed fixture intent")
        value = json.loads(self.run(["create", "-f", "-", "-o", "json", "--request-timeout=20s"], kernel.encoded(body)))
        kernel.native_uid(value)
        return value

    def pod(self, job_uid):
        kernel.identifier(job_uid)
        until = min(time.time() + 60, self.plan["deadline"])
        while True:
            raw = self.run(["get", "pods", "-n", "boss-dev", "-l", "boss.dev/request=" + self.plan["request"], "-o", "json", "--request-timeout=10s"])
            value = json.loads(raw)
            if not isinstance(value.get("items"), list):
                raise kernel.Unknown("native Pod collection incomplete")
            pods = value["items"]
            if len(pods) > 1:
                raise kernel.Unknown("multiple receiver Pod incarnations")
            if pods:
                pod = pods[0]
                owners = [owner for owner in pod["metadata"].get("ownerReferences", []) if owner.get("controller") is True]
                if len(owners) != 1 or owners[0].get("kind") != "Job" or owners[0].get("uid") != job_uid:
                    raise kernel.Unknown("native receiver belongs to another original Job")
                statuses = pod.get("status", {}).get("containerStatuses", [])
                if pod.get("status", {}).get("phase") == "Running" and len(statuses) == 1 and statuses[0].get("ready") is True:
                    return pod
                if pod.get("status", {}).get("phase") in ("Failed", "Succeeded"):
                    raise kernel.Unknown("receiver terminated before qualification")
            if time.time() >= until:
                raise kernel.Unknown("receiver readiness not observed within the original lifetime")
            time.sleep(1)

    def receiver(self, phase, binding):
        if phase not in ("controls", "collect"):
            raise kernel.Unknown("unknown receiver operation")
        kernel.identifier(binding["pod_uid"])
        # The executable checks its own DownwardAPI UID before reading the
        # synthetic authority. A same-name replacement cannot borrow it.
        raw = self.run(["exec", "-n", "boss-dev", binding["pod"], "-c", "receiver", "--",
                        "/usr/bin/python3", "-I", kernel.HELPER, phase, self.plan["request"],
                        self.plan["nonce"], binding["pod_uid"]])
        return json.loads(raw)

    def controls(self, binding):
        return self.receiver("controls", binding)

    def collect(self, binding):
        return self.receiver("collect", binding)

    def delete(self, kind, name, uid):
        self.target(kind, name)
        kernel.identifier(uid)
        prefix = "/apis/batch/v1/namespaces/boss-dev/jobs/" if kind == "Job" else "/api/v1/namespaces/boss-dev/services/"
        body = kernel.encoded({"apiVersion": "v1", "kind": "DeleteOptions", "preconditions": {"uid": uid}})
        raw = self.run(["delete", "--raw", prefix + name, "-f", "-"], body)
        return {"response_base64": base64.b64encode(raw).decode()}
