# files-root.sh — where packet attachment bytes live.
#
# One definition, sourced by everything that needs it, because this
# path has two independent readers that MUST agree:
#
#   infra/deploy-services.sh  writes it into boss-content-api's
#                             `[files] root`, so the service stores
#                             bytes there;
#   infra/backup.sh           copies it, so a restore still has them.
#
# If those two ever drift, backups silently stop covering the store
# while continuing to report success — and since the `file_refs` rows
# ride the pg_dump, the restored system insists every attachment
# exists at a path holding nothing. CLAUDE.md §9a: collapse a fact
# that would otherwise live twice, rather than leaving a comment
# asking the next person to keep them in sync.
#
# Objects land at `<root>/sha256/<hex>` — content-addressed, so the
# tree can never hold two names for the same bytes, and copying it is
# naturally idempotent.
BOSS_FILES_ROOT="${BOSS_FILES_ROOT:-/var/lib/boss/files}"
