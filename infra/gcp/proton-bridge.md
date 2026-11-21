# BOSS Bridge host

The accepted boss-gcp converge installs the pinned, signature-checked Bridge
release. Its dedicated non-root service keeps its state and local keychain in
`/var/lib/boss-bridge`. Both mail endpoints remain on localhost. This car retains
the existing account MFA and adds no tenant credential registry rows.

After deployment, David runs the accepted tree's `login` operation from an
interactive boss-gcp terminal:

```
sudo /var/lib/boss/gcp-root-prepared/bootstrap/infra/gcp/gcp-root-launch.sh run infra/gcp/proton-bridge.sh login
```

He enters `login agent-b@algedonic.dev` in Bridge.
Support mail routes to that account. David completes the existing account login
and MFA directly with Bridge; no account password or MFA seed belongs in BOSS,
the credential registry, a transcript, or this repository. Exit the CLI when done.
The control stops the daemon before opening its vault and restarts it afterwards.
Do not start another CLI against the running service.

An authorized operator then uses the registered `capture-bridge-credential`
operation. Its root-only local control stops and verifies the daemon, asks
`list` until the account has finished loading (a vault account reads `locked`
until then, and `info` prints nothing for it), asks `info` for the one connected
row by its index and then `exit`, validates one complete configuration, restarts the
service, and atomically writes `/etc/boss-bridge/adapter-credential.json` as root
0600 inside a root 0700 directory. On success it prints only the location and
byte count. A refusal names an allowlisted constant reason beside UNAVAILABLE;
CLI text, configuration values and other exception messages remain private.
A missing owner login (no account configuration), a second stale account (more
than one) and a configuration naming another account are three different
reasons, because each needs a different hand. The account list adds its own:
no account, every account signed out, and an account still loading or locked
when the 35 s wait ends (backlog 34d95817). The wrapper prints the child's
reason once and its own generic line only when the child died without one.
An unrecognized refusal stays generic. An unavailable outcome does not establish
whether a credential replacement reached durable storage.
Do not print, copy, paste, attach, or hand-transport that file. Bridge's generated
IMAP and SMTP password is one secret with two scopes; it is not the account
password. Registry rows belong to the separate tenant packet and contain the
location and capture time, never the value. A future local adapter must use an
approved local credential-read mechanism without weakening these permissions.

The signed-package rehearsal established the logged-out CLI path and the pinned
source's connected-account export path. It did not establish a real logged-in
export. Installation, David's login, real capture, endpoint verification and
independent evidence remain owed after convergence. Fixture tests do not pay
those obligations. No mail adapter or outbound transport is installed by this car.
