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
the one eunha builds. A migration whose work depends on the instance rather than
the schema — so far only the move of local signing keys into `keypairs` — is
applied from code at startup and records itself then; `mastodon:plan` lists
those separately from ones still to write.

### Signing keys

Mastodon 4.7.0 keeps local accounts' signing keys in `keypairs`, with the
private half encrypted the way Rails encrypts columns. Give eunha the same
secrets that Mastodon requires and it reads and writes that form, moving any
keys still in the old `accounts` columns on startup:

    ACTIVE_RECORD_ENCRYPTION__PRIMARY_KEY=...
    ACTIVE_RECORD_ENCRYPTION__KEY_DERIVATION_SALT=...

Without them, keys stay in `accounts.private_key`, which upstream still reads
(`Keypair.from_legacy_account`) — but a database whose keys have already moved
cannot be signed with, and eunha says so loudly at startup.

### HTTP signatures

Outgoing requests are signed with the draft-cavage signatures the network still
runs on, and double-knocked with [RFC 9421] HTTP Message Signatures when a peer
answers 400 or 401 — the order Mastodon 4.7 uses. Inbound requests are verified
either way: a `Signature-Input` alongside the `Signature` selects RFC 9421,
where the covered components must include the body's `content-digest` and the
signature must be fresh, just as the draft path requires a covered `digest` and
a recent `Date`. Both live in [feder].

[RFC 9421]: https://www.rfc-editor.org/rfc/rfc9421.html
[feder]: https://github.com/limeburst/feder

### Outstanding from 4.7.0

* **`mldsa44-jcs-2024` integrity proofs.** feder verifies FEP-8b32 proofs, but
  only `eddsa-jcs-2022`; the post-quantum suite needs an ML-DSA implementation,
  and upstream needs OpenSSL 3.5 for it too.
* **Ed25519 HTTP Message Signatures.** The RFC 9421 support here signs and
  verifies `rsa-v1_5-sha256`, which is what Mastodon uses for the keys it has;
  Ed25519 keys wait on the key types that would carry them.
* **Out-of-support version notifications.** `software_deprecations` and
  `software_updates.end_of_support` exist in the schema and stay empty: they
  describe Mastodon's release branches, which say nothing about whether the
  Mastodon version *eunha* implements is still supported.
  `mise run mastodon:status` answers that question instead.
