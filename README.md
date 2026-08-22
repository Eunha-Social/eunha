Eunha
=====

Rust re-implementation of Mastodon.

Eunha aims for 100% Mastodon database schema compatibility, so that Eunha can be a drop-in replacement on top of your existing Mastodon database.

We track the latest Mastodon release, and provide migration path from old Eunha database schema to updated Mastodon database schema.

It's not Eunha's goal to completely mimic Mastodon's feature set or its implementation detail, and Eunha may contain behavioral differences.


Contributing
------------

All Mastodon tables should go in the `public` schema, while tables needed for Eunha goes in the `eunha` schema.

Use mise for all tasks. See `mise.toml`.

Use [shadcn/ui](https://ui.shadcn.com) CLI when adding components. Don't hand-roll components.


Federation
----------

For all federation related tasks, we use [feder](https://github.com/limeburst/feder), and extend it when necessary.


Tracking Mastodon
-----------------

The Mastodon release eunha implements is recorded in `mastodon.toml` and
repeated as build metadata in `Cargo.toml`'s version, which `build.rs` checks
the two agree on. Releases are tagged the same way:

    v0.2.0+mastodon.4.7.0

Eunha's own version moves independently; the part after `+` names the Mastodon
release whose schema and API that build implements, and is what
`/api/v1/instance`, `/api/v2/instance` and NodeInfo report.

`eunha-schema` (`mise run mastodon:status`, `mastodon:plan`, `schema:check`)
does the tracking:

    mise run mastodon:status                 # is there a newer Mastodon release?
    mise run mastodon:plan --to v4.8.0       # what would adopting it involve?
    mise run schema:check                    # does this database match the target?

`schema:check` reads the live database back out of Postgres and diffs it against
upstream's `db/schema.rb` for the tracked release — tables, columns, types,
nullability, index names and uniqueness, and foreign keys including `ON DELETE`.
It is the test for the 100%-compatibility claim, so run it after any migration.
`--schema-rb path/to/mastodon/db/schema.rb` compares against a local checkout
instead of GitHub.

### Adopting a release

1. `mise run mastodon:plan --to vX.Y.Z` lists the Rails migrations upstream
   added since the tracked release.
2. Write one eunha migration reproducing them, ending with an
   `INSERT INTO public.schema_migrations` of the versions it covers — a
   migration eunha deliberately does not implement is left out of that list, so
   that a Mastodon booted on the database still runs it. `--sql` prints the
   insert.
3. Update `mastodon.toml` and `Cargo.toml`, then `mise run schema:check`
   against a database that has run the new migration.
4. Rehearse it against real data before deploying. Migrations run at startup,
   so an instance gets one attempt at them:

       scripts/rehearse_migration.sh postgres://user@localhost/seoul_earth

   That clones the database (reading only), runs the pending migrations over
   the clone as the server would, and reports every table whose row count
   changed plus the schema check. Anything that moves rows it should not is
   visible there rather than in production.

`public.schema_migrations` is what makes a database self-describing: it is
seeded for everything through 4.6.0 by `007_mastodon_schema_versions.sql`, and
`scripts/migrate_from_mastodon.sh` refuses a dump whose newest migration is not
the one eunha builds.

### Outstanding from 4.7.0

The schema is complete; these behaviours are not, and are why
`mise run mastodon:plan` still reports one migration outstanding:

* **Local keypairs still live on `accounts`.** 4.7 moves them into the
  `keypairs` table, keyed by `local_fragment`, with private keys encrypted at
  rest. Eunha still signs from `accounts.private_key`, so upstream's
  `20260702144128_migrate_local_account_keypairs` is deliberately not recorded
  as applied — it would blank the keys eunha signs with.
* **Handle changes.** 4.7 treats an actor's `id` as the primary identifier and
  renames remote accounts that change handle instead of duplicating them.
  Eunha renders the `invalid_handle` accounts such a database may already
  contain, but never marks one itself.
* **RFC9421 HTTP Message Signatures**, Ed25519 signatures, FEP-8b32 integrity
  proof verification, and FEP-521a — federation work that belongs in feder.
* **`Link` attachments (FEP-8967)** as the source of preview cards.
* **Out-of-support version notifications**, which is what `software_deprecations`
  and `software_updates.end_of_support` are for.
