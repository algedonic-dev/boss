# Migrated StepPlugin declarations

These six immutable declaration files are copied byte for byte from
`infra/platform/step-plugins/` at source
`a8f41524af1051bcec274d20683ffd8f6f178735`, before the private enrollment
data-only addition. Together with the independently retained withdrawn
fixtures, they describe the active rows produced by historical migrations.

The database test compares every declared column with the migrations. New
bundle rows must not be added here: they belong to current registry data,
and the seeding test proves their insertion, event count, idempotence and
conservation of the independently checked historical rows. Production
seeders never load this directory.
