// Membership is the module declaration itself; the gate reads these same paths.
// Partitions use measured/declared LPT costs; only the FULL gate proves runtime.
#[path = "../dev_reclaim_git_phase.rs"]
mod dev_reclaim_git_phase;
#[path = "../dev_scratch_reclaim_sh.rs"]
mod dev_scratch_reclaim_sh;
#[path = "../longhorn_disk_sh.rs"]
mod longhorn_disk_sh;
#[path = "../longhorn_disk_tags_sh.rs"]
mod longhorn_disk_tags_sh;
#[path = "../longhorn_drain_policy_sh.rs"]
mod longhorn_drain_policy_sh;
#[path = "../longhorn_ledger_sh.rs"]
mod longhorn_ledger_sh;
#[path = "../longhorn_selector_policy_sh.rs"]
mod longhorn_selector_policy_sh;
#[path = "../mounted_filesystem_evidence.rs"]
mod mounted_filesystem_evidence;
#[path = "../node_trim_check_sh.rs"]
mod node_trim_check_sh;
#[path = "../read_longhorn_setting_sh.rs"]
mod read_longhorn_setting_sh;
#[path = "../read_postgres_placement_sh.rs"]
mod read_postgres_placement_sh;
#[path = "../volume_replica_history_sh.rs"]
mod volume_replica_history_sh;
