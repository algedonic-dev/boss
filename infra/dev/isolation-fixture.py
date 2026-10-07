#!/usr/bin/python3 -I
"""Synthetic causal filesystem control, never a deployed Pod attestation.

Only generated fake credentials and a loopback test receiver are used.
The jail contains explicitly copied reviewed tools, no host home/cache.
"""
import os
from pathlib import Path
import re
import shutil
import subprocess


def copy_tool(jail, tool):
    destination = jail / str(tool).lstrip("/")
    destination.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(Path(tool).resolve(), destination)
    destination.chmod(0o755)
    result = subprocess.run(["/usr/bin/ldd", str(tool)], capture_output=True, check=False)
    if result.returncode:
        raise ValueError("fixture tool dependency observation failed")
    for library in re.findall(r"(/[\w./+-]+)", result.stdout.decode()):
        source = Path(library)
        if not source.is_file():
            raise ValueError("fixture dependency is not a regular tool file")
        target = jail / library.lstrip("/")
        target.parent.mkdir(parents=True, exist_ok=True)
        if not target.exists():
            shutil.copyfile(source.resolve(), target)
            target.chmod(0o755)
    for directory, _, _ in os.walk(jail):
        Path(directory).chmod(0o755 if Path(directory).name != "work" else 0o777)


def candidate(argv, env, cwd, jail=None):
    def enter():
        os.chroot(jail)
        os.chdir("/" + str(cwd.relative_to(jail)))
        os.setgroups([])
        os.setgid(1500)
        os.setuid(65534)
    return subprocess.run(argv, env=env, cwd=cwd, preexec_fn=enter if jail is not None else None,
                          capture_output=True, timeout=30, close_fds=True, check=False)
