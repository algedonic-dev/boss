# Runbook: access recovery — getting back in when a key dies

**Why this lives in the repo:** the repo mirrors to GitHub, so these
procedures survive total cluster loss. No secrets appear here — only
where each door's out-of-band path is and what it requires.

## The trust map (what unlocks what)

| Anchor | Recovers | Requires |
|---|---|---|
| Google account (GCP console) | boss-gcp SSH (new key via metadata / OS Login; serial console) | Google 2FA |
| Physical access (home) | minipc login; Talos nodes via console (maintenance-mode re-bootstrap) | being there |
| boss-gcp SSH | forge token file, WireGuard into 10.20.0.0/24, pipeline | the key or the GCP path above |
| minipc (physical or SSH-via-jump) | forge admin CLI, dockerized kubectl (`/tmp/kc.yaml`), registry | boss-gcp jump or physical |
| Kanidm `idm_admin` (`kanidmd recover-account`, password in cluster secret `kanidm/kanidm-idm-admin`) | the human door (playground OIDC/passkeys) | kubectl → the chain above |
| Cloudflare account | CF Access + tunnel for playground | CF credentials |
| GitHub account | code mirror | GitHub credentials |

Bottom line today: **the Google account and physical access to the
LAN boxes are the two out-of-band anchors.** Everything else chains
from them.

## Recovery procedures

1. **Lost laptop / SSH key** → GCP console → Compute Engine →
   boss-gcp → add a new public key to instance metadata (or serial
   console). From boss-gcp, jump to the minipc requires the minipc
   authorizing the NEW key: do that via physical login, or in advance
   (see hardening below).
2. **Lost kubeconfig/talosconfig** → minipc holds a working
   kubeconfig at `/tmp/kc.yaml` (dockerized kubectl). For Talos
   client config, re-export from the escrow copy (below) or console
   the nodes.
3. **Locked out of the human door** → recover `idm_admin` per the
   table, re-register credentials for the person account.
4. **Cluster gone entirely** → rebuild from `infra/cluster/manifests/`
   (in-tree) + nightly pg dumps on boss-gcp
   (`/var/backups/boss-cluster-pg`, keep 21) + images rebuilt from
   this repo. GitHub mirror covers repo loss.

## Hardening still owed (the gaps this runbook cannot paper over)

- **Break-glass keypair** — David generates a second keypair OFFLINE
  (hardware token or cold storage; the private key must never touch
  a session transcript or a shared machine); its public half gets
  authorized on boss-gcp AND the minipc. Until then, the minipc's
  only non-physical door chains through one key.
- **Config escrow** — encrypted archive of `~/talos-homelab/v2/`
  (kubeconfig, talosconfig) + WireGuard configs, stored where the
  Google account can reach it (Secret Manager or a private GCS
  bucket). Single-laptop copies are the sharpest edge today.
- **Offsite backup leg** — replicate the pg dumps from boss-gcp to a
  GCS bucket; today boss-gcp is both the backup basket and a single
  point of loss.
