#!/usr/bin/env python3
"""Render the worker fence's fixed markers from the estate, before apply.

Only addresses/port are substituted, never a caller command or a policy
allow override. This prepares source; Cilium enforcement requires native
policy readback and the bounded fake-only rollout's actual network probes.
"""
import ipaddress
import json
from pathlib import Path
import sys
import tomllib
from urllib.parse import urlsplit

PRIVATE = ("0.0.0.0/8", "10.0.0.0/8", "100.64.0.0/10", "127.0.0.0/8",
           "169.254.0.0/16", "172.16.0.0/12", "192.168.0.0/16",
           "224.0.0.0/4", "240.0.0.0/4", "::/128", "::1/128",
           "::ffff:0:0/96", "fc00::/7", "fe80::/10", "ff00::/8")


def render(template, estate):
    forge = ipaddress.ip_address(estate["forge_host"])
    forge_url = urlsplit(estate["forge_url"])
    sor_url = urlsplit(estate["sor_url"])
    if forge_url.scheme not in ("http", "https") or forge_url.hostname != str(forge) or forge_url.username or forge_url.password or forge_url.path not in ("", "/") or forge_url.query or forge_url.fragment:
        raise ValueError("worker forge route must be the estate's fixed uncredentialed address")
    sor = ipaddress.ip_address(sor_url.hostname)
    if sor == forge:
        raise ValueError("worker forge exception cannot be the system of record")
    forge_cidr = str(ipaddress.ip_network(f"{forge}/{forge.max_prefixlen}"))
    sor_cidr = str(ipaddress.ip_network(f"{sor}/{sor.max_prefixlen}"))
    deny = []
    for cidr in PRIVATE:
        rule = {"cidr": cidr}
        if forge in ipaddress.ip_network(cidr):
            rule["except"] = [forge_cidr]
        deny.append(rule)
    deny.append({"cidr": sor_cidr})
    public = [{"cidr": cidr, "except": [p for p in PRIVATE if ipaddress.ip_network(p).version == ipaddress.ip_network(cidr).version]}
              for cidr in ("0.0.0.0/0", "::/0")]
    markers = {"__FORGE_CIDR__": forge_cidr,
               "__FORGE_PORT__": str(forge_url.port or (443 if forge_url.scheme == "https" else 80)),
               "__PRIVATE_DENY_CIDRS__": json.dumps(deny),
               "__PUBLIC_ALLOW_CIDRS__": json.dumps(public)}
    for marker, value in markers.items():
        if template.count(marker) != 1:
            raise ValueError("worker fence marker absent or duplicated: " + marker)
        template = template.replace(marker, value)
    if "__" in template:
        raise ValueError("worker fence has an unresolved marker")
    return template


def main():
    source, estate_path, output = map(Path, sys.argv[1:])
    text = render(source.read_text(), tomllib.loads(estate_path.read_text()))
    output.write_text(text)


if __name__ == "__main__":
    main()
