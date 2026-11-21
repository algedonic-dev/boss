#!/usr/bin/env python3
"""Fixed health argv, bounded collection, safe diagnostic output; never a shell.

Talos v1.13.8: logs.go passes numeric --tail to the API. dmesg.go's
--tail is boolean (new messages with --follow), NOT a numeric tail.
Consequently dmesg is finite/non-following and locally bounded explicitly.
"""
from collections import deque
import os
import re
import selectors
import signal
import subprocess
import sys
import time

LINES = 200
BYTES = 262144  # per channel; enforced while receiving, not after capture
SECONDS = 25  # ops_talosctl also has its existing container-side 20s timeout
SERVICES = ("machined", "kubelet", "etcd", "containerd", "apid", "trustd")
LOGS = ("machined", "kubelet", "etcd")
READS = {"pressure-memory": "/proc/pressure/memory", "pressure-io": "/proc/pressure/io",
         "pressure-cpu": "/proc/pressure/cpu", "meminfo": "/proc/meminfo",
         "uptime": "/proc/uptime", "loadavg": "/proc/loadavg"}
ETCD_READS = {"etcd-members": ("etcd", "members"), "etcd-status": ("etcd", "status"),
              "etcd-alarms": ("etcd", "alarm", "list")}


def argv(reading, service):
    if reading == "service" and service in SERVICES:
        return ["service", service, "status"]
    if reading == "logs" and service in LOGS:
        return ["logs", service, "--tail", str(LINES)]
    if service:
        raise ValueError("service is not allowed for this reading")
    if reading == "services":
        return ["service"]
    if reading == "dmesg":
        return ["dmesg"]
    if reading in READS:
        return ["read", READS[reading]]
    if reading in ETCD_READS:
        return list(ETCD_READS[reading])
    raise ValueError("reading or service is not allowlisted")


def safe_lines(raw):
    # Whole sensitive lines/PEM blocks are withheld, not partially replaced.
    # Long opaque strings are withheld too: diagnostics are not a credential
    # transport. Unknown binary/control bytes are never replayed to a terminal.
    kept = deque(maxlen=LINES)
    redacted = 0
    pem = False
    lines = raw.decode("utf-8", errors="replace").splitlines()
    for line in lines:
        if "-----BEGIN " in line:
            pem = True
        sensitive = pem or re.search(
            r"(?i)password|passwd|secret|token|authorization|credential|private.?key|serial|wwn|uuid", line
        ) or re.search(r"[A-Za-z0-9_+/=-]{32,}|[a-z]+://[^\s/@]+:[^\s/@]+@", line)
        if sensitive:
            redacted += 1
            kept.append("[sensitive diagnostic line withheld]")
        else:
            kept.append("".join(c if c == "\t" or c.isprintable() else "?" for c in line))
        if "-----END " in line:
            pem = False
    return list(kept), len(lines), redacted


def collect(command, seconds=SECONDS, byte_limit=BYTES):
    """Only the owned child group is terminated; the container timeout remains.

    A hung child or inherited pipe cannot extend this reader indefinitely.
    Partial safe stdout AND stderr survive refusals/timeouts with their status.
    """
    buffers = {1: bytearray(), 2: bytearray()}
    reason = None
    with subprocess.Popen(command, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                          stderr=subprocess.PIPE, start_new_session=True) as child:
        with selectors.DefaultSelector() as poll:
            for fd, stream in [(1, child.stdout), (2, child.stderr)]:
                os.set_blocking(stream.fileno(), False)
                poll.register(stream, selectors.EVENT_READ, fd)
            end = time.monotonic() + seconds
            while poll.get_map():
                left = end - time.monotonic()
                if left <= 0:
                    reason = "time limit"
                    break
                for key, _ in poll.select(min(left, 0.1)):
                    chunk = os.read(key.fileobj.fileno(), 4096)
                    if not chunk:
                        poll.unregister(key.fileobj)
                        continue
                    target = buffers[key.data]
                    room = byte_limit - len(target)
                    target.extend(chunk[:room])
                    if len(chunk) > room:
                        reason = "byte limit"
                        break
                if reason:
                    break
            if reason:
                os.killpg(child.pid, signal.SIGKILL)
            try:
                status = child.wait(timeout=max(0.01, end - time.monotonic()))
            except subprocess.TimeoutExpired:
                reason = "time limit"
                os.killpg(child.pid, signal.SIGKILL)
                status = child.wait()
    return buffers, status, reason


def main(args):
    validate = args[:1] == ["--validate"]
    if validate:
        args = args[1:]
    try:
        fixed = argv(args[0], args[1])
        if not validate and (len(args) < 4 or args[2] != "--"):
            raise ValueError("missing owned container door")
    except (ValueError, IndexError):
        print("talos-node-health: REFUSED — reading/service is not allowlisted", file=sys.stderr)
        return 78
    if validate:
        # Derive the target restriction from the same fixed argv. The shell
        # must not keep a second inventory of which reading requires etcd.
        print("talos-control-plane" if fixed[0] == "etcd" else "talos")
        return 0
    buffers, status, reason = collect([*args[3:], *fixed])
    etcd = fixed[0] == "etcd"
    incomplete = False
    for fd, output in [(1, sys.stdout), (2, sys.stderr)]:
        lines, count, redacted = safe_lines(buffers[fd])
        incomplete = incomplete or redacted > 0 or count > LINES
        print(f"talos-node-health: channel={fd} bytes={len(buffers[fd])} lines={count} "
              f"retained={len(lines)} redacted={redacted} clipped={count > LINES}", file=output)
        for line in lines:
            print(line, file=output)
    if etcd:
        # Talos can return success after warning about a partial response.
        # Its empty alarm output is not evidence of an empty cluster alarm set.
        print("talos-node-health: single-node observation; completeness=UNKNOWN health=UNKNOWN"
              + (" alarm_set=UNKNOWN" if fixed[1:] == ["alarm", "list"] else ""))
        if reason or status or incomplete or buffers[2]:
            print("talos-node-health: partial observation; completeness=UNKNOWN health=UNKNOWN",
                  file=sys.stderr)
    if reason:
        print(f"talos-node-health: FAILED — {reason}; partial bounded readings above, not complete",
              file=sys.stderr)
        return 1
    if status:
        print(f"talos-node-health: FAILED — talosctl/container exit {status}", file=sys.stderr)
        return 1
    if etcd and (incomplete or buffers[2]):
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
