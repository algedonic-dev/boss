#!/usr/bin/python3 -I
"""Image-owned input/exec fence. No controller credential enters this image."""
import json
import os
from pathlib import Path
import argparse
import base64
import hashlib
import selectors
import signal
import stat
import subprocess
import sys
import time
import uuid

WORKSPACE = Path("/workspace")
LIMITS = {"bundle": 128 * 1024 * 1024, "prompt": 4 * 1024 * 1024,
          "start": 128 * 1024, "artifact": 128 * 1024 * 1024}


def identity():
    """Kernel generation, never a caller-provided counter or candidate file."""
    pod_uid = os.environ["AGENT_POD_UID"]
    stat = Path("/proc/1/stat").read_text()
    # comm may contain spaces or ')'; fields begin after its last closing ')'.
    ticks = int(stat[stat.rindex(")") + 2:].split()[19])
    return {"pod_uid": pod_uid, "pid1_start_ticks": ticks,
            "pid_namespace_inode": os.stat("/proc/self/ns/pid").st_ino,
            "boot_id": Path("/proc/sys/kernel/random/boot_id").read_text().strip()}


def require_fence(expected):
    observed = identity()
    if not isinstance(expected, dict) or set(expected) != set(observed) or expected != observed:
        raise ValueError("actual Pod/kernel incarnation differs; input was not opened")
    return observed


def strict_json(raw):
    def unique(pairs):
        result = {}
        for key, value in pairs:
            if key in result:
                raise ValueError("duplicate JSON key " + key)
            result[key] = value
        return result
    return json.loads(raw, object_pairs_hook=unique)


def digest(raw):
    return hashlib.sha256(raw).hexdigest()


def private_write(path, raw):
    # One typed name, exclusive creation, no links or overwrite.
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    with os.fdopen(fd, "wb") as output:
        output.write(raw)
        output.flush()
        os.fsync(output.fileno())
    directory = os.open(path.parent, os.O_RDONLY | os.O_DIRECTORY)
    try:
        os.fsync(directory)
    finally:
        os.close(directory)


def read_regular(path, limit):
    for ancestor in [path.parent, *path.parents]:
        if ancestor == Path("/"):
            break
        if stat.S_ISLNK(os.lstat(ancestor).st_mode):
            raise ValueError("artifact has a linked ancestor")
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(fd, "rb") as source:
        info = os.fstat(source.fileno())
        if not stat.S_ISREG(info.st_mode) or info.st_size > limit:
            raise ValueError("artifact is special or oversized")
        raw = source.read(limit + 1)
        if len(raw) > limit:
            raise ValueError("artifact grew over its byte bound")
        return raw


def emit(value):
    sys.stdout.write(json.dumps(value, separators=(",", ":")) + "\n")
    sys.stdout.flush()


def event_ack(raw, request):
    expected = {"schema": 1, "run": request["run"], "prompt_sha256": request["prompt_sha256"],
                "snapshot_sha256": request["snapshot"]["sha256"], "start_read": True}
    matches = []
    for line in raw.splitlines():
        try:
            event = strict_json(line)
        except (ValueError, UnicodeError):
            continue
        item = event.get("item", {})
        if event.get("type") == "item.completed" and item.get("type") == "agent_message":
            text = item.get("text", "")
            if text.startswith("BOSS_START_ACK "):
                matches.append(strict_json(text[len("BOSS_START_ACK "):].strip()))
    if matches != [expected]:
        raise ValueError("native assistant did not supply one exact START acknowledgment")
    return expected


def import_snapshot(workspace, snapshot, remaining):
    bundle = read_regular(workspace / "source.bundle", LIMITS["bundle"])
    if digest(bundle) != snapshot["sha256"] or len(bundle) != snapshot["bytes"]:
        raise ValueError("snapshot differs from the prepared bytes")
    tree = workspace / "tree"
    # Bundles contain objects/refs, never host config, helpers or hooks.
    git_env = {"PATH": "/usr/local/bin:/usr/bin:/bin", "HOME": str(workspace / "home"),
               "GIT_CONFIG_NOSYSTEM": "1", "GIT_CONFIG_GLOBAL": "/dev/null"}
    for args in [["clone", "--no-checkout", str(workspace / "source.bundle"), str(tree)],
                 ["-C", str(tree), "-c", "core.hooksPath=/dev/null", "checkout", "--detach", snapshot["commit"]],
                 ["-C", str(tree), "remote", "remove", "origin"],
                 ["-C", str(tree), "update-ref", "refs/remotes/origin/main", snapshot["main"]]]:
        result = subprocess.run(["/usr/bin/git", *args], env=git_env, capture_output=True, timeout=max(0.01, remaining()))
        emit({"kind": "preparation", "argv": args, "exit": result.returncode,
              "stdout": base64.b64encode(result.stdout).decode(), "stderr": base64.b64encode(result.stderr).decode()})
        if result.returncode:
            raise ValueError("bundle import failed; complete native git error was streamed")
    for ref, expected in [("HEAD", snapshot["commit"]), ("HEAD^{tree}", snapshot["tree"])]:
        result = subprocess.run(["/usr/bin/git", "-C", str(tree), "rev-parse", ref], env=git_env,
                                capture_output=True, timeout=max(0.01, remaining()))
        if result.returncode or result.stdout.decode().strip() != expected:
            emit({"kind": "preparation-error", "exit": result.returncode,
                  "stdout": base64.b64encode(result.stdout).decode(), "stderr": base64.b64encode(result.stderr).decode()})
            raise ValueError("imported commit/tree readback differs")
    return tree


def validate_argv(argv):
    if not isinstance(argv, list) or not argv or not isinstance(argv[0], str) or Path(argv[0]).parent != Path("/usr/local/bin") or any(not isinstance(a, str) or "\x00" in a for a in argv):
        raise ValueError("harness argv is not an image-owned executable array")
    return argv


def managed_plan(request):
    plan = request.get("managed_cache")
    keys = {"schema", "run", "nonce", "credential", "issuer", "principal", "generation", "binding"}
    binding_keys = {"job_uid", "pod", "pod_uid", "container_id"}
    if not isinstance(plan, dict) or set(plan) != keys or type(plan["schema"]) is not int or plan["schema"] != 1:
        raise ValueError("managed cache requires the bound private lease plan")
    for key in ["run", "nonce"]:
        try:
            identity = uuid.UUID(plan[key])
            if not identity.int or str(identity) != plan[key]:
                raise ValueError()
        except (ValueError, TypeError, AttributeError):
            raise ValueError("managed cache lease identity is invalid") from None
    binding = plan["binding"]
    if plan["run"] != request.get("run") or not isinstance(binding, dict) or set(binding) != binding_keys:
        raise ValueError("managed cache run/incarnation differs")
    if binding["pod_uid"] != os.environ.get("AGENT_POD_UID"):
        raise ValueError("managed cache belongs to another Pod")
    values = [plan[key] for key in ["credential", "issuer", "principal", "generation"]] + list(binding.values())
    if any(not isinstance(value, str) or not value or any(c.isspace() or ord(c) < 32 for c in value) for value in values):
        raise ValueError("managed cache metadata is absent or malformed")
    return plan


def directory_fd(path):
    path = Path(path)
    if not path.is_absolute():
        raise ValueError("private directory must be absolute")
    current = os.open("/", os.O_RDONLY | os.O_DIRECTORY)
    try:
        for component in path.parts[1:]:
            if component in [".", ".."]:
                raise ValueError("private directory traversal is refused")
            following = os.open(component, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=current)
            os.close(current)
            current = following
        return current
    except Exception:
        os.close(current)
        raise


def issuer_bytes(root):
    """Only the fixed read-only mount, including its pinned atomic-writer generation."""
    descriptor = directory_fd(root)
    selected = descriptor
    try:
        try:
            target = os.readlink("auth.json", dir_fd=descriptor)
        except OSError:
            target = None
        if target is not None:
            if target != "..data/auth.json":
                raise ValueError("issuer projection link differs")
            generation = os.readlink("..data", dir_fd=descriptor)
            if not generation.startswith("..") or "/" in generation or generation in [".", ".."]:
                raise ValueError("issuer projection generation is unsafe")
            selected = os.open(generation, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=descriptor)
        fd = os.open("auth.json", os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=selected)
        with os.fdopen(fd, "rb") as source:
            info = os.fstat(source.fileno())
            if not stat.S_ISREG(info.st_mode) or info.st_nlink != 1 or info.st_mode & 0o222 or info.st_size > 256 * 1024:
                raise ValueError("issuer input is not a bounded read-only regular file")
            raw = source.read(256 * 1024 + 1)
            if len(raw) != info.st_size or len(raw) > 256 * 1024:
                raise ValueError("issuer input changed during materialization")
            return raw
    finally:
        if selected != descriptor:
            os.close(selected)
        os.close(descriptor)


def prepare_managed_login(request, state, issuer=Path("/harness/login")):
    """No bytes or private parse errors enter an acknowledgment or diagnostic."""
    managed_plan(request)
    if request.get("login_file") != "auth.json" or request.get("home_env") != "CODEX_HOME":
        raise ValueError("managed cache layout differs")
    descriptor = None
    try:
        descriptor = directory_fd(state)
        info = os.fstat(descriptor)
        if info.st_uid != os.geteuid() or info.st_mode & 0o777 != 0o700:
            raise ValueError("worker cache directory is not private")
        raw = issuer_bytes(issuer)
        fd = os.open("auth.json", os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600, dir_fd=descriptor)
        with os.fdopen(fd, "wb") as output:
            output.write(raw)
            output.flush()
            os.fsync(output.fileno())
        os.fsync(descriptor)
    except Exception:
        raise ValueError("private managed cache preparation refused") from None
    finally:
        if descriptor is not None:
            os.close(descriptor)


def run_harness(request, workspace, remaining):
    managed = request.get("home_env") == "CODEX_HOME" and request.get("login_file") == "auth.json"
    if managed:
        managed_plan(request)
    prompt = read_regular(workspace / "prompt.txt", LIMITS["prompt"])
    if digest(prompt) != request["prompt_sha256"]:
        raise ValueError("prompt differs from the prepared bytes")
    argv = validate_argv(request["argv"])
    tree = import_snapshot(workspace, request["snapshot"], remaining)
    home = workspace / "home"
    state = workspace / "state"
    artifacts = workspace / "artifacts"
    for directory in [home, state, artifacts]:
        directory.mkdir(mode=0o700)
    # Login item/layout must come from the issuer-owned binding, not discovery.
    login_file = request["login_file"]
    if not isinstance(login_file, str) or "/" in login_file or login_file in ["", ".", ".."]:
        raise ValueError("login item is not the declared single filename")
    if managed:
        prepare_managed_login(request, state)
    else:
        os.symlink("/harness/login/" + login_file, state / login_file)
    variable = request["home_env"]
    if not variable or not all(c.isupper() or c == "_" for c in variable) or variable in ["HOME", "PATH"]:
        raise ValueError("harness state variable is invalid")
    env = {"HOME": str(home), "TMPDIR": str(workspace / "tmp"), "PATH": "/usr/local/bin:/usr/bin:/bin", variable: str(state)}
    (workspace / "tmp").mkdir(mode=0o700)
    with (artifacts / "native.stdout").open("xb") as stdout, (artifacts / "native.stderr").open("xb") as stderr:
        with (workspace / "prompt.txt").open("rb") as prompt_file:
            child = subprocess.Popen(argv, cwd=tree, env=env, stdin=prompt_file, stdout=subprocess.PIPE,
                                     stderr=subprocess.PIPE, start_new_session=True)
        selector = selectors.DefaultSelector()
        for stream, name, output in [(child.stdout, "stdout", stdout), (child.stderr, "stderr", stderr)]:
            os.set_blocking(stream.fileno(), False)
            selector.register(stream, selectors.EVENT_READ, (name, output))
        sequence = 0
        exited_at = None
        ending = "native_exit"
        acknowledged = False
        while selector.get_map():
            status = child.poll()
            if status is not None and exited_at is None:
                exited_at = time.monotonic()
            if remaining() <= 0:
                ending = "deadline"
                break
            # Inherited-open pipes never turn a native exit into an unbounded wait.
            if exited_at is not None and time.monotonic() - exited_at >= 2:
                ending = "native_exit_with_partial_pipe_tail"
                break
            for key, _ in selector.select(min(0.05, max(0, remaining()))):
                raw = os.read(key.fileobj.fileno(), 8192)
                if not raw:
                    selector.unregister(key.fileobj)
                    continue
                name, output = key.data
                output.write(raw)
                output.flush()
                os.fsync(output.fileno())
                sequence += 1
                emit({"kind": "native-stream", "sequence": sequence, "stream": name,
                      "data": base64.b64encode(raw).decode()})
                if name == "stdout" and not acknowledged:
                    try:
                        ack = event_ack(read_regular(artifacts / "native.stdout", LIMITS["artifact"]), request)
                        emit({"kind": "start-ack", "value": ack})
                        acknowledged = True
                    except (ValueError, UnicodeError):
                        pass
                if output.tell() > LIMITS["artifact"]:
                    ending = "output_byte_bound"
                    break
            if ending == "output_byte_bound":
                break
        selector.close()
        # This request only stops the local group. Escaped descendants are
        # ended by the independently bounded, UID-deleted cluster Job.
        try:
            os.killpg(child.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        status = child.wait(timeout=2)
    raw = read_regular(artifacts / "native.stdout", LIMITS["artifact"])
    runtime = None
    completed = False
    for line in raw.splitlines():
        try:
            event = strict_json(line)
            if event.get("type") == request["runtime_event"]:
                candidate = event.get(request["runtime_key"])
                if runtime is not None and runtime != candidate:
                    raise ValueError("ambiguous native runtime")
                runtime = candidate
            if event.get("type") == request["completed_event"]:
                completed = True
        except (json.JSONDecodeError, UnicodeError):
            pass
    metadata = {"schema": 1, "runtime": runtime, "exit": status, "completed": completed, "ending": ending,
                "transcript_pattern": request["transcript_pattern"]}
    private_write(artifacts / "metadata.json", json.dumps(metadata).encode())
    ack = event_ack(raw, request)
    return ack


def collect(workspace):
    artifacts = workspace / "artifacts"
    metadata = strict_json(read_regular(artifacts / "metadata.json", LIMITS["start"]))
    runtime = metadata["runtime"]
    if not isinstance(runtime, str) or not runtime or not all(c.isalnum() or c == "-" for c in runtime):
        raise ValueError("native runtime unavailable or unsafe")
    pattern = metadata["transcript_pattern"].replace("{runtime}", runtime)
    if pattern.startswith("/") or ".." in pattern.split("/") or "\\" in pattern:
        raise ValueError("transcript pattern is not bounded to this state directory")
    matches = list((workspace / "state").glob(pattern))
    if len(matches) != 1:
        raise ValueError("transcript is missing or ambiguous")
    paths = {"stdout": artifacts / "native.stdout", "stderr": artifacts / "native.stderr",
             "last_message": artifacts / "last-message.txt", "transcript": matches[0]}
    result = {**metadata, "artifacts": {}}
    for name, path in paths.items():
        raw = read_regular(path, LIMITS["artifact"])
        result["artifacts"][name] = {"data": base64.b64encode(raw).decode(), "sha256": digest(raw), "bytes": len(raw)}
    return result


def recover(workspace, request):
    """Read available evidence after compute loss; never invent completion."""
    result = {"schema": 1, "completion": "interrupted", "artifacts": {}, "missing": {}}
    artifacts = workspace / "artifacts"
    for name, filename in [("stdout", "native.stdout"), ("stderr", "native.stderr"),
                           ("last_message", "last-message.txt"), ("native_metadata", "metadata.json")]:
        try:
            raw = read_regular(artifacts / filename, LIMITS["artifact"])
            result["artifacts"][name] = {"data": base64.b64encode(raw).decode(), "bytes": len(raw), "sha256": digest(raw)}
        except (OSError, ValueError) as error:
            result["missing"][name] = str(error)
    try:
        raw = base64.b64decode(result["artifacts"]["stdout"]["data"])
        runtimes = set()
        for line in raw.splitlines():
            try:
                event = strict_json(line)
                if event.get("type") == request["runtime_event"]:
                    runtimes.add(event[request["runtime_key"]])
            except (ValueError, KeyError, UnicodeError):
                continue
        if len(runtimes) != 1:
            raise ValueError("native runtime missing or ambiguous")
        runtime = runtimes.pop()
        if not isinstance(runtime, str) or not runtime or not all(c.isalnum() or c == "-" for c in runtime):
            raise ValueError("unsafe native runtime")
        pattern = request["transcript_pattern"].replace("{runtime}", runtime)
        if pattern.startswith("/") or ".." in pattern.split("/") or "\\" in pattern:
            raise ValueError("unsafe transcript pattern")
        matches = list((workspace / "state").glob(pattern))
        if len(matches) != 1:
            raise ValueError("transcript missing or ambiguous")
        raw = read_regular(matches[0], LIMITS["artifact"])
        result["artifacts"]["transcript"] = {"data": base64.b64encode(raw).decode(), "bytes": len(raw), "sha256": digest(raw)}
    except (OSError, ValueError, KeyError) as error:
        result["missing"]["transcript"] = str(error)
    return result


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("operation", choices=["identity", "bundle", "prompt", "start", "collect", "recover"])
    parser.add_argument("--fence")
    parser.add_argument("--seconds", type=float, default=0)
    args = parser.parse_args()
    if args.operation == "identity":
        if args.fence or args.seconds or not sys.stdin.isatty() and sys.stdin.buffer.read(1):
            raise ValueError("identity carries no input or caller generation")
        emit(identity())
        return
    # Compare immutable API PodUID/kernel PID1 generation BEFORE opening bytes.
    require_fence(strict_json(args.fence or "null"))
    deadline = time.monotonic() + args.seconds
    remaining = lambda: deadline - time.monotonic()
    if remaining() <= 0:
        raise ValueError("operation has no remaining controller bound")
    if args.operation == "collect":
        emit(collect(WORKSPACE))
        return
    if args.operation == "recover":
        raw = sys.stdin.buffer.read(LIMITS["start"] + 1)
        if len(raw) > LIMITS["start"]:
            raise ValueError("recovery request exceeds its byte bound")
        emit(recover(WORKSPACE, strict_json(raw)))
        return
    limit = LIMITS[args.operation]
    raw = sys.stdin.buffer.read(limit + 1)
    if len(raw) > limit:
        raise ValueError("typed input exceeds its byte bound")
    if args.operation in ["bundle", "prompt"]:
        name = "source.bundle" if args.operation == "bundle" else "prompt.txt"
        private_write(WORKSPACE / name, raw)
        emit({"sha256": digest(raw), "bytes": len(raw)})
    else:
        emit({"kind": "result", "value": run_harness(strict_json(raw), WORKSPACE, remaining)})


if __name__ == "__main__":
    try:
        main()
    except Exception as error:
        emit({"kind": "refused", "error": str(error)})
        raise
