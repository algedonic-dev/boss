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
import json


def write_bun_lifecycle_fixture(work):
    """A zero-dependency, offline hook using only caller-generated fake paths.

    The marker proves the real install executed JS; the copied fake effect is
    a filesystem control, never privileged network use or a worker attestation.
    """
    (work / "package.json").write_text(json.dumps({
        "name": "boss-synthetic-lifecycle", "version": "1.0.0",
        "scripts": {"postinstall": "bun lifecycle.js"},
    }))
    (work / "lifecycle.js").write_text('''import {readFileSync, writeFileSync} from "node:fs";
const result = {lifecycle: true, javascript: 6 * 7, read: false, effect: false};
try {
    const material = readFileSync(process.env.FAKE_PATH, "utf8");
    if (!material.startsWith("BOSS_SYNTHETIC_FIXTURE:")) throw new Error("fake material required");
    result.read = true;
    writeFileSync(process.env.FAKE_EFFECT, material);
    result.effect = true;
} catch (error) {
    if (error.code !== "ENOENT" && error.code !== "EACCES") throw error;
}
writeFileSync("lifecycle-observed.json", JSON.stringify(result));
''')


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
