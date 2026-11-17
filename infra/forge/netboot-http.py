#!/usr/bin/env python3
"""netboot-http — serve the forge's network boot over plain HTTP: the
iPXE script and the ONE declared Talos kernel and initramfs (backlog
ee31ab73; infra/forge/netboot.sh carries the design).

WHY ITS OWN SERVER and not `python3 -m http.server`: what this answers
is what a blank node on the LAN boots, so the rule "only the declared
bytes are served" is held HERE, where the bytes leave, and not only by
the converge that put them on disk:

  * at start it opens the kernel and the initramfs, hashes what it
    opened, and REFUSES TO START (exit 1, naming both shas) unless each
    is the sha256 the declaration names. It then serves from the
    descriptors it verified, so a file replaced on disk afterwards is
    never what goes out — the next converge restarts it on new bytes;
  * it answers exactly three paths — /boot.ipxe and the pair under
    NETBOOT_PREFIX — with GET or HEAD. Anything else is a 404: no
    listing, no query string, no path resolved against a directory;
  * it serves at most MAX_CLIENTS connections at once, each a thread; one
    past that is closed unanswered and logged (review bcff47dd N5), and a
    PXE client that gets no script boots its next device;
  * it runs as a systemd dynamic user with a read-only view of the host
    (forge-netboot-http.service), and logs every request to the journal.

Everything comes from the environment the converge renders
(/var/lib/boss/netboot/serve.env): NETBOOT_BIND, NETBOOT_PORT (0 picks a
free port, for the tests), NETBOOT_IMAGES, NETBOOT_SCRIPT,
NETBOOT_PREFIX, NETBOOT_KERNEL_SHA256, NETBOOT_INITRAMFS_SHA256. Its
first stdout line names where it listens.
"""

import hashlib
import http.server
import os
import sys
import threading

CHUNK = 1 << 20
# A handful of nodes boot from here at once; a LAN client can open
# connections faster than they finish, and each is a thread holding up to
# Handler.timeout. The first line the server prints names this number.
MAX_CLIENTS = 16


def die(line):
    print(f"netboot-http: {line}", file=sys.stderr, flush=True)
    sys.exit(1)


def need(name):
    value = os.environ.get(name, "")
    if not value:
        die(f"REFUSED: {name} is not set — the converge renders it into serve.env; nothing is served")
    return value


def verified(path, want):
    """Open `path`, hash what was opened, and keep the descriptor only if
    it is the declared bytes."""
    try:
        fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
    except OSError as e:
        die(f"REFUSED: cannot open {path}: {e} — nothing is served")
    digest = hashlib.sha256()
    offset = 0
    while True:
        block = os.pread(fd, CHUNK, offset)
        if not block:
            break
        digest.update(block)
        offset += len(block)
    got = digest.hexdigest()
    if got != want:
        die(f"REFUSED: {path} is sha256 {got}, the declaration says {want} — nothing is served")
    return fd, offset


class BoundedServer(http.server.ThreadingHTTPServer):
    """ThreadingHTTPServer with a thread cap: a connection past
    MAX_CLIENTS is closed in the accept loop, never handed a thread."""

    daemon_threads = True

    def __init__(self, *args, **kwargs):
        self.slots = threading.BoundedSemaphore(MAX_CLIENTS)
        super().__init__(*args, **kwargs)

    def process_request(self, request, client_address):
        if not self.slots.acquire(blocking=False):
            print(
                f"netboot-http: REFUSED {client_address[0]}: {MAX_CLIENTS} clients are already being served",
                file=sys.stderr,
                flush=True,
            )
            self.shutdown_request(request)
            return
        try:
            super().process_request(request, client_address)
        except BaseException:
            # No thread took the slot, so nothing else will release it.
            self.slots.release()
            raise

    def process_request_thread(self, request, client_address):
        try:
            super().process_request_thread(request, client_address)
        finally:
            self.slots.release()


def main():
    bind = need("NETBOOT_BIND")
    port = int(need("NETBOOT_PORT"))
    images = need("NETBOOT_IMAGES")
    prefix = need("NETBOOT_PREFIX").rstrip("/")
    want = {
        "kernel-amd64": need("NETBOOT_KERNEL_SHA256"),
        "initramfs-amd64.xz": need("NETBOOT_INITRAMFS_SHA256"),
    }
    with open(need("NETBOOT_SCRIPT"), "rb") as f:
        script = f.read()
    if not script.startswith(b"#!ipxe\n"):
        die("REFUSED: the iPXE script does not start with #!ipxe — nothing is served")

    # path -> (descriptor, size) or bytes
    routes = {"/boot.ipxe": script}
    for name, sha in want.items():
        routes[f"{prefix}/{name}"] = verified(os.path.join(images, name), sha)

    class Handler(http.server.BaseHTTPRequestHandler):
        server_version = "boss-netboot"
        sys_version = ""
        # A client that stops reading holds one thread for at most this.
        timeout = 120

        def answer(self, body_out):
            route = routes.get(self.path)
            if route is None:
                self.send_error(404)
                return
            size = len(route) if isinstance(route, bytes) else route[1]
            self.send_response(200)
            self.send_header("Content-Type", "application/octet-stream")
            self.send_header("Content-Length", str(size))
            self.end_headers()
            if not body_out:
                return
            if isinstance(route, bytes):
                self.wfile.write(route)
                return
            fd, offset = route[0], 0
            while offset < size:
                block = os.pread(fd, min(CHUNK, size - offset), offset)
                if not block:
                    break
                self.wfile.write(block)
                offset += len(block)

        def do_GET(self):
            self.answer(True)

        def do_HEAD(self):
            self.answer(False)

    server = BoundedServer((bind, port), Handler)
    host, bound = server.server_address[:2]
    print(
        f"netboot-http: serving {', '.join(sorted(routes))} "
        f"(kernel {want['kernel-amd64']}, initramfs {want['initramfs-amd64.xz']}) "
        f"to at most {MAX_CLIENTS} clients at once on {host}:{bound}",
        flush=True,
    )
    server.serve_forever()


if __name__ == "__main__":
    main()
