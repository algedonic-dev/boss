#!/usr/bin/python3 -I
"""UID-conditioned deletion using the existing dev-session credential.

There is no listener or worker-callable proxy. Only the trusted
controller constructs the endpoint, namespace, name and observed UID.
"""
import base64
import http.client
import ipaddress
import json
from pathlib import Path
import re
import ssl
from urllib.parse import urlsplit


class DeleteTransport:
    def __init__(self, host, port, certificate, token):
        try:
            ipaddress.ip_address(host)
        except ValueError:
            if not re.fullmatch(r"[A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)*", host):
                raise ValueError("invalid existing Kubernetes API hostname") from None
        if not 1 <= int(port) <= 65535:
            raise ValueError("invalid existing Kubernetes API port")
        self.host = host
        self.port = int(port)
        self.certificate = certificate
        self.token = token

    def delete_job(self, name, uid, record):
        if not re.fullmatch(r"dev-build-[0-9a-f]{16}", name) or not uid or not re.fullmatch(r"[A-Za-z0-9_-]+", uid):
            raise ValueError("retirement needs the controller-observed Job name and UID")
        path = "/apis/batch/v1/namespaces/boss-dev/jobs/" + name
        body = json.dumps({"apiVersion": "v1", "kind": "DeleteOptions",
                           "preconditions": {"uid": uid}, "propagationPolicy": "Foreground"}).encode()
        receipt = {"method": "DELETE", "namespace": "boss-dev", "job": name, "job_uid": uid,
                   "path": path, "body_base64": base64.b64encode(body).decode("ascii"),
                   "completion": "unproven"}
        connection = None
        try:
            context = ssl.create_default_context(cafile=str(self.certificate))
            credential = self.token.read_text().strip()
            if not credential or any(c in credential for c in "\r\n\0"):
                raise ValueError("existing dev-session credential is absent or malformed")
            connection = http.client.HTTPSConnection(self.host, self.port, context=context, timeout=120)
            # http.client does not consult proxy environment or follow
            # redirects. Authority never appears in argv or a receipt.
            connection.request("DELETE", path, body=body,
                               headers={"Authorization": "Bearer " + credential, "Content-Type": "application/json"})
            response = connection.getresponse()
            receipt["http_status"] = response.status
            raw = response.read()
            receipt.update(http_status=response.status, completion="observed",
                           response_base64=base64.b64encode(raw).decode("ascii"))
            if not 200 <= response.status < 300:
                raise RuntimeError("UID-conditioned Job retirement refused by API; no unconditioned fallback")
            return json.loads(raw)
        except (OSError, ValueError, http.client.HTTPException) as error:
            receipt["error_kind"] = type(error).__name__
            partial = getattr(error, "partial", None)
            if isinstance(partial, bytes):
                receipt["response_base64"] = base64.b64encode(partial).decode("ascii")
            raise RuntimeError("UID-conditioned Job retirement unavailable; outcome retained, no retry") from error
        finally:
            if connection is not None:
                connection.close()
            record(receipt)


def existing_session(read):
    # Read exactly nonsecret context/server fields through the SAME
    # published client; SSH sessions need not inherit service env.
    def observed(argv):
        result = read(argv)
        if result.returncode:
            raise ValueError("existing Kubernetes authority could not be observed")
        return result.stdout
    if observed(["config", "current-context"]).strip() != b"in-cluster":
        raise ValueError("retirement requires the existing dev-session context")
    endpoint = urlsplit(observed(["config", "view", "--minify", "-o", "jsonpath={.clusters[0].cluster.server}"]).decode().strip())
    if endpoint.scheme != "https" or not endpoint.hostname or endpoint.username or endpoint.password or endpoint.query or endpoint.fragment or endpoint.path not in ("", "/"):
        raise ValueError("existing Kubernetes endpoint is not a bounded HTTPS authority")
    identity = json.loads(observed(["auth", "whoami", "-o", "json"]))
    if identity.get("status", {}).get("userInfo", {}).get("username") != "system:serviceaccount:boss-dev:dev-session":
        raise ValueError("observed Kubernetes identity is not the existing dev-session authority")
    root = Path("/var/run/secrets/kubernetes.io/serviceaccount")
    return DeleteTransport(endpoint.hostname, endpoint.port or 443, root / "ca.crt", root / "token")
