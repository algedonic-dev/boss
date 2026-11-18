# `boss` CLI

The single operator + developer entry point for BOSS. Wraps the
jobs API, the train and gate protocol, the audit log,
and the agent runtime so day-to-day work goes through one
interface instead of a tour of `journalctl` / `psql` / `cargo`
incantations.

Install, from the repository root:

```sh
cargo install --path crates/orchestrators/boss-cli --bin boss
```

## Commands

The command list is the binary's own: run `boss --help` for every
subcommand, and `boss <subcommand> --help` for its flags. This README
used to carry a table of them, which fell behind the CLI it described
(backlog `d83cbd3f`); the help text cannot, because it is generated
from the same definitions that parse the arguments.

The day-to-day operator playbook lives in
[docs/runbooks/operator.md](../../../docs/runbooks/operator.md).
Break-glass procedures and disk-space recovery are documented
there. The fresh-box setup walkthrough lives in
[docs/runbooks/dev-environment-bootstrap.md](../../../docs/runbooks/dev-environment-bootstrap.md).
