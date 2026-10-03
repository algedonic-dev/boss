BOSS RECOVERY KIT {{VERSION}} -- README
======================================

This file is in the clear and holds no secret value. It was written by
infra/forge/recovery-kit.sh (backlog c1bb822e) when the kit was cut:

    cut at      {{TAKEN_AT}}
    cut on      the forge, {{FORGE_HOST}}, from the checkout {{CHECKOUT}}
    kit         boss-recovery-kit-{{VERSION}}.tar.gpg  (beside this file;
                named ...-{{VERSION}}-INCOMPLETE.tar.gpg if items were MISSING)
    manifest    boss-recovery-kit-{{VERSION}}[-INCOMPLETE].MANIFEST.txt

WHAT THIS IS
------------
One encrypted file holding what the estate cannot re-derive: the forge's
repositories and database, the forge host's untreed credentials, the
cluster's operator credentials, the Talos secrets bundle (the cluster's
PKI -- without it no node can join the existing cluster), and the key to
the offsite bucket that holds the cluster's data. The MANIFEST beside it
lists every item by name, size and sha256, and names every item that was
MISSING when the kit was cut. Read it first: a MISSING item is not here.

The passphrase is not written anywhere by BOSS. It was typed only into
gpg's own prompt on the machine that wrote this stick. Where it lives
beyond one person's memory is its keeper's decision, and that decision is
part of this kit: a kit whose passphrase nobody can produce is not a kit.


RESTORE ORDER
=============
Restore only what is lost, but in this order -- each step needs the ones
above it.

0. OPEN AND VERIFY THE KIT (any machine with gpg; nothing else is needed)

     mkdir -m 700 kit && cd kit      # ideally on a RAM disk
     gpg --decrypt ../boss-recovery-kit-{{VERSION}}*.tar.gpg > kit.tar
     sha256sum kit.tar               # or: shasum -a 256 kit.tar

   The hash must equal `tar_sha256` in the MANIFEST. If it does not, the
   stick is damaged or this is not the kit the MANIFEST describes: stop.

     tar -xf kit.tar                 # -> boss-recovery-kit-{{VERSION}}/

   Everything below names paths inside that directory. Delete the
   plaintext when you are done with it.

1. DECIDE WHAT IS LOST. The estate has three layers, restored in order:
   (a) the operator's hold on the cluster, (b) the forge host -- Forgejo,
   the CI runner, the ops runner -- which holds the repository every
   other step is built from, and (c) the cluster and its data. A layer
   that still answers is not restored; test before you rebuild.

2. OPERATOR CREDENTIALS -- secrets/talosconfig, secrets/kubeconfig

   Place them on the operator host (the forge, or any host with talosctl
   and kubectl) as /etc/boss-ops/talosconfig and /etc/boss-ops/kubeconfig,
   root:root, mode 0600. Test:

     talosctl --talosconfig /etc/boss-ops/talosconfig -n <control-plane address> version
     kubectl --kubeconfig /etc/boss-ops/kubeconfig get nodes

   If both answer, the cluster is alive: skip step 5.

3. THE FORGE -- forge/forge-<stamp>.tar.gz

   A `forgejo dump`: forgejo-db.sql (the database, taken through
   Forgejo's own connection -- the consistent copy), repos/ (every
   repository), data/ (conf, jwt, ssh keys) and app.ini (its secrets).
   On a host running the Forgejo container at the version it ran before:
     - repos/          -> /data/git/repositories
     - data/           -> /data/gitea
     - app.ini         -> /data/gitea/conf/app.ini
     - forgejo-db.sql  -> sqlite3 /data/gitea/gitea.db < forgejo-db.sql
   all owned by the container's git user; then, inside the container,
   `forgejo admin regenerate hooks` and `forgejo doctor check --all`.
   Restore the database from the SQL, never from a raw gitea.db: a copy
   of a live SQLite file can be torn, and restores into a corrupt
   Forgejo that looks healthy.

4. THE FORGE HOST'S OWN CREDENTIALS, then its converge

     runner/.runner        -> the forgejo-runner unit's WorkingDirectory
                              (the MANIFEST says where it was read from)
     sudoers/*             -> /etc/sudoers.d/  root:root 0440, then
                              `visudo -c` BEFORE you log out

   Then check the repository out as the forge user's ~/boss and run the
   one bootstrap: `sudo infra/forge/install.sh`. It installs every unit
   the forge runs (the converge, the ops runner, the backup, the CI
   reaper); after it the host converges itself every ten minutes.

5. THE CLUSTER -- only if it is gone, or a node must be (re)joined

   talos/secrets.yaml is the Talos secrets bundle. With it, new machine
   configs join THE EXISTING cluster rather than founding a new one:

     talosctl gen config <cluster name> https://<endpoint>:6443 \
         --with-secrets talos/secrets.yaml

   then apply each node's declared patch from the repository,
   infra/cluster/talos/patches/<node>.yaml, and let the manifests under
   infra/cluster/manifests/ converge. talos/controlplane.yaml is a
   control-plane node's live config as it stood when the kit was cut:
   a reference for the cluster name, endpoint and every setting the
   patches do not declare. It carries the same secrets.

6. THE CLUSTER'S DATA -- gcs/sa.json, gcs/bucket

   The service-account key to the offsite bucket that holds the nightly
   Postgres dumps (boss-<stamp>.sql.gz) and file-store archives
   (boss-files-<stamp>.tar.gz). The bucket is the ONE offsite copy:
   boss-gcp keeps none since 2026-10-01.

   The live estate records remote archive validation with:
     boss ops forge check-gcs-backup --wait
   It also runs weekly. Its receipt names the newest database object's
   generation, metadata, downloaded byte count and SHA-256, then proves
   gzip integrity and the completion trailer. It reads the existing
   Secret; no key is copied from this kit to perform the routine check.
   This proves remote archive integrity, not a database restore. Older
   boss-gcp dumps may have no bucket copy: dumps modified before
   2026-09-11 09:10 UTC need David's decision on a signed reclaim plan
   that lists each dump's mtime. A successful check authorizes no reclaim.

     gcloud auth activate-service-account --key-file gcs/sa.json
     gcloud storage ls "gs://$(cat gcs/bucket)/"

   Fetch the newest dump and the archive stamped beside it, the stamp
   read from that list:

     gcloud storage cp "gs://$(cat gcs/bucket)/boss-<stamp>.sql.gz" .
     gcloud storage cp "gs://$(cat gcs/bucket)/boss-files-<stamp>.tar.gz" .

   Restore those two. Put the key back where the backup reads it:

     kubectl -n boss create secret generic boss-gcs-offsite \
         --from-file=sa.json=gcs/sa.json --from-file=bucket=gcs/bucket

7. CUT A NEW KIT as soon as the estate stands again, and rotate what this
   one carried: a kit that has been opened is a copy of every credential
   in it.


WHAT IS NOT ON THIS STICK
-------------------------
The passphrase. Hardware keys and passkeys. The Google, GitHub and
Cloudflare accounts and their second factors -- the printed recovery
sheet maps those, under its road
"The accounts outside the system: Google, GitHub, Cloudflare"
(its source is infra/recovery/re-entry.toml). The cluster's
etcd state (the Postgres dump is the data; the repository is the
configuration). The credential broker's root tokens, which live in
cluster Secrets and are re-placed by hand.


WRITING ANOTHER COPY
--------------------
Any machine on the estate's WireGuard tunnel can write one, from a
checkout of the repository. It needs:

  - to have joined the tunnel (a peer of the hub whose overlay address is
    HUB_IP in infra/cluster/wireguard/setup-hub.sh). The forge
    ({{FORGE_HOST}}) is reached by jumping through the hub, which routes
    the LAN;
  - an SSH key the hub and the forge accept for your user;
  - on the forge, ONE of:
      . your user named in /etc/sudoers.d/boss-recovery-kit, which the
        forge's converge renders for its checkout's owner and which
        grants {{READER}} and nothing else; or
      . a dedicated key (BOSS_KIT_SSH_KEY=<private key> when writing)
        whose line in the forge user's ~/.ssh/authorized_keys is
          command="set -f; exec sudo -n {{READER}} $SSH_ORIGINAL_COMMAND",restrict <public key>
        -- placing it is a person's signed act, never a converge's;
  - gpg 2.2.7 or newer, bash, ssh, and sha256sum or shasum;
  - the stick mounted and writable.

Then:

  1. File the `cut-recovery-kit` ops verb on the forge. Its answer names
     the kit version and the sha256 the writer had when it was cut.
  2. From the root of your checkout (check that
     infra/forge/write-recovery-kit.sh hashes to that sha256):

       bash infra/forge/write-recovery-kit.sh <version> <stick mount path>

It writes the MANIFEST and this README beside the kit. It streams the kit
through gpg, so the plaintext exists only in the pipe, with AES256 and a
SHA512 key derivation iterated to OpenPGP's maximum. It syncs, then
unmounts and re-mounts the stick, so the read-back reads the stick and
not memory. It decrypts the stick's copy -- gpg asks for the passphrase
again, which is the proof you can produce it -- and names the file a kit
only if the copy's hash equals the MANIFEST's. Finally it tells the
forge, which records the write on the packet and drops the kit from RAM.
When gpg's prompt offers to save the passphrase in a keychain, saving it
stores it on that machine.
