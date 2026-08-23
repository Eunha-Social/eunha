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

`schema:check` reads the live database back out of Postgres and compares it
against a **reference**: the structure of a database that Mastodon's own
ActiveRecord built from its `db/schema.rb`. Comparing Postgres to Postgres means
comparing everything Postgres knows — tables, column types, nullability and
defaults, the columns an index actually covers, every constraint *by name* as
well as by definition, sequences, and view definitions — rather than what a
parser of ours believes a Ruby file means. It is the test for the
100%-compatibility claim, and it runs on every `cargo test`.

Two things are deliberately not compared, because they record how a database
came to be rather than what it is, and `schema.rb` cannot express either:

* **Sequences left behind by dropped tables.** Mastodon creates one sequence per
  snowflake-id table by hand, so nothing owns it and dropping the table leaves it
  behind — `encrypted_messages_id_seq` has outlived its table since 2022. Every
  Mastodon that migrated through that period has it; one installed fresh today
  does not. Eunha matches the former, because that is what it stands in for. A
  sequence whose table *does* exist, or one the reference has and the database
  lacks, is still reported.
* **Sequence ownership.** `quotes` was created with a serial id and later moved
  to `timestamp_id`, so a migrated Mastodon owns that sequence and a freshly
  loaded one does not.

Three files under `mastodon/` support it, all regenerated together by
`mise run schema:build-reference`:

* `schema.rb` — upstream's own file, vendored verbatim.
* `schema.sql` — a `pg_dump` of a database built from it, for reading and
  diffing.
* `schema.json` — the same database's structure as the checker sees it, which
  is what the test compares against so that it needs neither Ruby nor a
  database of its own.

Building the reference runs Mastodon's `schema.rb` through the real ActiveRecord
schema DSL. That needs Ruby, but not Mastodon: `activerecord` and `pg`, not its
thousand-gem bundle.

### Adopting a release

1. `mise run mastodon:plan --to vX.Y.Z` lists the Rails migrations upstream
   added since the tracked release, and the deliberate divergences that now need
   re-examining against it.
2. Write one eunha migration reproducing them, ending with an
   `INSERT INTO public.schema_migrations` of the versions it covers — a
   migration eunha deliberately does not implement is left out of that list, so
   that a Mastodon booted on the database still runs it. `--sql` prints the
   insert.
3. Update `mastodon.toml` and `Cargo.toml`, replace `mastodon/schema.rb` with
   that release's, and run `mise run schema:build-reference`. The reference's
   diff is the schema delta you are adopting.
4. Re-examine each divergence and move its `reviewed_for` forward in
   `divergences.toml`; the suite fails until every entry has been looked at.
5. `mise run schema:check` against a database that has run the new migration.
6. Rehearse it against real data before deploying. An instance gets one attempt
   at a migration:

       scripts/rehearse_migration.sh postgres://user@localhost/seoul_earth

   That clones the database (reading only), runs the pending migrations over
   the clone as the server would, and reports every table whose row count
   changed plus the schema check. Anything that moves rows it should not is
   visible there rather than in production.

### Migrations

Migrations are applied by `eunha migrate`, not by starting the server. A
migration takes as long as it takes and some are destructive — 4.7's account
merge deletes rows — so running them from a deploy script, before the new binary
starts, means a failure is found with the old version still serving rather than
with nothing serving at all.

Starting the server checks instead: an instance whose database is behind its
binary refuses to serve and says so, rather than running queries against a shape
that has moved. `eunha migrate --check` answers the same question without
applying anything, and exits non-zero when something is pending, so a deploy
script can gate on it.

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

What gets signed matches Mastodon rather than merely satisfying the spec: the
same covered headers in the same order, `(request-target)` last and carrying
any query string, `Content-Type` bound to deliveries, and no `alg` parameter on
RFC 9421 signatures. Order is not a correctness matter — a verifier rebuilds
from the header list it is given — but emitting what the rest of the network
emits keeps eunha clear of anything that verifies more strictly than it should.

[RFC 9421]: https://www.rfc-editor.org/rfc/rfc9421.html
[feder]: https://github.com/limeburst/feder

### Update notices

Mastodon polls an update server for newer releases and for the end of support
of the branch it runs, and records both in `software_updates` and
`software_deprecations`. Eunha asks the same server the same question about the
Mastodon release *it implements*: eunha builds 4.7.0's schema and serves its
API, so when that branch stops receiving fixes, what eunha reproduces is what
is going out of support.

The request carries the Mastodon version being asked about and eunha's own
`User-Agent`; it does not claim to be Mastodon. Set `software_update_url` to an
empty string to turn the check off, or to another server to ask it instead.

### API entity parity

The schema check answers whether eunha's database matches Mastodon's. The
entity check answers the API half: a client reads fields by name, so a missing
one breaks it and an unexpected one is a divergence nobody decided on.

`mastodon/entities.json` records what each of Mastodon's REST entities carries,
and tests fetch real responses from a running eunha and compare. It is built
from two sources because neither is sufficient alone — `app/serializers/rest`
decides what is actually emitted, and `app/javascript/mastodon/api_types` states
plainly which fields are optional, where the serializers hide that behind `if:`
conditions. 4.7.0's instance serializer emits `icon` and `wrapstodon` that the
TypeScript does not mention, so the serializers are the authority on what
exists.

    mise run entities:build        # re-record from a Mastodon checkout

That reads a clone at `~/Git/mastodon` (`MASTODON_REPO` to point elsewhere) at
the tracked tag rather than its working tree, fetching tags if the tag is
missing. Mastodon is not a submodule: 424MB of history for 468KB of files that
only matter when adopting a release, and a submodule bump's diff is a SHA,
whereas the diff of what is recorded here *is* the change being adopted.

### Differential testing against a live Mastodon

The entity check compares eunha to what upstream's serializers *say*. This one
asks upstream directly: the same request goes to both servers and the responses
are compared.

    scripts/differential_test.sh http://localhost:3001 SOME_TOKEN

That brings up Mastodon in Docker — the official image, because building it from
source on macOS means libidn, OpenSSL headers for `hiredis-client`, libvips, and
a `pg` gem that segfaults against Postgres 18, all of which the image has already
solved — mints tokens, and compares what a client actually does: 31 reads, nine
writes, and the interaction verbs (favourite, boost, bookmark, pin, follow,
block, mute, and their undos).

The stack runs Sidekiq as well as the web process. Without a worker nothing
Mastodon defers ever happens, and some of that shows in the API — a home feed
stays `regenerating?` and answers 206 forever — which reads as a difference from
eunha when it is a missing worker. An unfaithful reference invents findings.

Nothing in it encodes what the answer should be, which is the point: a rule
misread while writing a test would be misread in the test too. It found seven
differences the source-reading had missed, all of them fields nested inside
objects the entity extraction never descended into.

It compares values too, but only under `configuration.*` — the limits an
instance states about itself, where two servers genuinely should agree. That
came second, after a shape-only comparison passed `max_display_name_length: 30`
against Mastodon's 40, both being integers. Comparing values found the media
description limit still advertised at Mastodon's older 1500 rather than 10,000.

Everything else stays shape-only on purpose. `followers_count`, ids and
timestamps depend on each instance's data, and comparing them would bury real
findings under differences that mean nothing.

### Deliberate divergences

Eunha aims for behavioural parity, so a difference from Mastodon is either a bug
or a decision. The decisions live in `divergences.toml`, one entry each, saying
what Mastodon does, what eunha does instead, why, and which test would fail if
that stopped being true.

They are recorded as data rather than prose because prose is not checked and so
stops being true. `cargo test` reads that file: an entry whose evidence has gone
missing fails, and — the part that matters over time — every entry carries the
Mastodon release it was last judged against, so **adopting a newer release fails
the suite until each divergence has been re-examined**. A divergence that made
sense against one release is not automatically right against the next; upstream
may have adopted the same idea, changed what is being diverged from, or ruled it
out. `mise run mastodon:plan` prints them when adopting, so the question is
asked at the moment it can be answered.

At the time of writing there are four, covering integrity proofs on outgoing
activities, the invite tree, what the update check asks about, and when the
local-keypair migration is recorded. Read the file rather than this paragraph:
the file is the one that has to stay true.

### Outstanding from 4.7.0

Nothing. Signing HTTP Message Signatures with Ed25519 or ML-DSA keys remains
unimplemented, but so is it in Mastodon: local accounts' HTTP signatures are
RSA on both sides. Both algorithms are verified inbound.
