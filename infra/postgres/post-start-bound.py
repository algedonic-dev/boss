#!/usr/bin/env python3
"""Own the fixed post-start command and all of its descendants until reaped."""

import ctypes
import os
from pathlib import Path
import signal
import shutil
import subprocess
import sys
import time

MODES = {"--pending": 15, "--apply": 600}


def run_owned(argv, seconds, term_grace=5, kill_grace=1):
    # Orphans must return here, not accumulate under the container's PID 1.
    if ctypes.CDLL(None, use_errno=True).prctl(36, 1, 0, 0, 0) != 0:
        print("post-start: cannot own descendant cleanup", file=sys.stderr)
        return 70
    child = subprocess.Popen(argv, start_new_session=True)
    timed_out = False
    try:
        result = child.wait(timeout=seconds)
    except subprocess.TimeoutExpired:
        timed_out = True
        result = 124
        print("post-start: execution deadline exceeded; debt remains unpaid", file=sys.stderr)

    def send(sig):
        try:
            os.killpg(child.pid, sig)
            return True
        except ProcessLookupError:
            return False

    def drained():
        child.poll()
        while True:
            try:
                pid, _ = os.waitpid(-1, os.WNOHANG)
            except ChildProcessError:
                return True
            if pid == 0:
                return False

    def wait_cleanup(duration):
        deadline = time.monotonic() + duration
        while not drained():
            if time.monotonic() >= deadline:
                return False
            time.sleep(min(0.01, max(0, deadline - time.monotonic())))
        return True

    # Even a successful leader cannot leave work executing outside its receipt.
    remaining = send(signal.SIGTERM)
    if remaining and result == 0 and not timed_out:
        result = 125
        print("post-start: command left descendants; refusing success", file=sys.stderr)
    if not wait_cleanup(term_grace):
        send(signal.SIGKILL)
        if not wait_cleanup(kill_grace):
            print("post-start: descendant cleanup incomplete; debt remains unpaid", file=sys.stderr)
            return 125
    return result if result >= 0 else 128 - result


def main():
    if len(sys.argv) == 3 and sys.argv[1] == "--apply-after-precheck":
        if not sys.argv[2].isdigit() or not 1 <= int(sys.argv[2]) <= 255:
            print("post-start: invalid precheck outcome", file=sys.stderr)
            return 2
        # Descriptor 3 is the already-open, unlinked private capture. There is
        # no caller-selected filename or command to evaluate. Both phases ride
        # boss-chore's existing full-output file attachment, including recovery.
        print("post-start: initial precheck exit " + sys.argv[2], flush=True)
        with os.fdopen(3, "rb") as precheck:
            shutil.copyfileobj(precheck, sys.stdout.buffer)
        sys.stdout.buffer.flush()
        result = run_owned(["/bin/bash", str(Path(__file__).with_name("post-start.sh")), "--apply"], MODES["--apply"])
        print("post-start: apply outcome exit " + str(result), flush=True)
        return result
    if len(sys.argv) != 2 or sys.argv[1] not in MODES:
        print("post-start: expected --pending or --apply", file=sys.stderr)
        return 2
    mode = sys.argv[1]
    return run_owned(["/bin/bash", str(Path(__file__).with_name("post-start.sh")), mode], MODES[mode])


if __name__ == "__main__":
    sys.exit(main())
