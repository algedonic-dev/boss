"""Trusted fixed entry: capture identity bytes and incarnation before compile.

This entry is the trust boundary, not a claim that hashing its own later path
attests its compiled bytes. No caller-supplied paths, captures or commands.
"""
import hashlib
import json
import os
import pathlib
import sys
import types

sys.dont_write_bytecode = True


class CaptureUnavailable(Exception):
    pass


def load_identity():
    path = pathlib.Path(__file__).resolve().with_name("admission-reader-identity.py")
    key = lambda stat: (stat.st_dev, stat.st_ino, stat.st_size, stat.st_mtime_ns, stat.st_ctime_ns)
    with path.open("rb") as stream:
        before = os.fstat(stream.fileno())
        source = stream.read(1024 * 1024 + 1)
        after = os.fstat(stream.fileno())
    if not source or len(source) > 1024 * 1024 or len(source) != before.st_size:
        raise CaptureUnavailable("identity_file_bound")
    if key(before) != key(after) or key(before) != key(path.stat()):
        raise CaptureUnavailable("identity_file_changed")
    capture = types.MappingProxyType({"sha256": hashlib.sha256(source).hexdigest(),
                                     "identity": key(before)})
    code = compile(source, str(path), "exec")
    module = types.ModuleType("captured_admission_identity")
    module.__file__ = str(path)

    class CapturedLoader:
        def compiled_capture(self):
            caller = sys._getframe(1)
            if caller.f_code is not code or caller.f_globals is not module.__dict__:
                return None
            return capture

    module.__loader__ = CapturedLoader()
    exec(code, module.__dict__)
    return module


def main():
    reason = "identity_method_changed"
    try:
        return load_identity().main()
    except CaptureUnavailable as error:
        if str(error) in {"identity_file_bound", "identity_file_changed"}:
            reason = str(error)
    except OSError:
        reason = "identity_file_unavailable"
    except (ValueError, TypeError, SyntaxError):
        pass
    print(json.dumps({"schema": "boss.admission-reader-identity.v1",
                      "scope": "installed_reader_identity", "state": "unavailable",
                      "history_verdict": "unavailable", "reason": reason},
                     separators=(",", ":"), sort_keys=True))
    return 0 if sys.argv[1:] == ["--inside"] else 4


if __name__ == "__main__":
    sys.exit(main())
