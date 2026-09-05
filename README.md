Eunha
=====

Rust re-implementation of Mastodon.

Eunha aims for 100% Mastodon database schema compatibility, so that Eunha can
be a drop-in replacement on top of your existing Mastodon database.

We track the latest Mastodon release, and provide migration path from old Eunha
database schema to updated Mastodon database schema.

It's not Eunha's goal to completely mimic Mastodon's feature set or its
implementation detail, and Eunha may contain behavioral differences.


Contributing
------------

All Mastodon tables should go in the `public` schema, while tables needed for
Eunha goes in the `eunha` schema.

Use mise for all tasks. See `mise.toml`.

Use [shadcn/ui] CLI when adding components. Don't hand-roll components.

[shadcn/ui]: https://ui.shadcn.com


Federation
----------

For all federation related tasks, we use [feder], and extend it when necessary.

The extension eunha designs for the places ActivityPub scales badly is recorded
in [PROTOCOL.md](./PROTOCOL.md): the dereference storm a boost sets off, the
absence of backfill, and identity that cannot outlive a hostname. It is a design
record rather than a description of what eunha does today, and it says which is
which.

[feder]: https://github.com/limeburst/feder


Tracking Mastodon
-----------------

The Mastodon release eunha implements is recorded in `mastodon.toml` and
repeated as build metadata in `Cargo.toml`'s version, which `build.rs` checks
the two agree on. Releases are tagged the same way:

~~~~
v0.2.0+mastodon.4.7.1
~~~~

Eunha's own version moves independently; the part after `+` names the Mastodon
release whose schema and API that build implements, and is what
`/api/v1/instance`, `/api/v2/instance` and NodeInfo report.

`eunha-schema` (`mise run mastodon:status`, `mastodon:plan`, `schema:check`)
does the tracking:

~~~~
mise run mastodon:status                 # is there a newer Mastodon release?
mise run mastodon:plan --to v4.8.0       # what would adopting it involve?
mise run schema:check                    # does this database match the target?
~~~~

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

 -  **Sequences left behind by dropped tables.** Mastodon creates one sequence
    per snowflake-id table by hand, so nothing owns it and dropping the table
    leaves it behind — `encrypted_messages_id_seq` has outlived its table since
    2022. Every Mastodon that migrated through that period has it; one
    installed fresh today does not. Eunha matches the former, because that is
    what it stands in for. A sequence whose table *does* exist, or one the
    reference has and the database lacks, is still reported.
 -  **Sequence ownership.** `quotes` was created with a serial id and later
    moved to `timestamp_id`, so a migrated Mastodon owns that sequence and a
    freshly loaded one does not.

Three files under `mastodon/` support it, all regenerated together by
`mise run schema:build-reference`:

 -  `schema.rb` — upstream's own file, vendored verbatim.
 -  `schema.sql` — a `pg_dump` of a database built from it, for reading and
    diffing.
 -  `schema.json` — the same database's structure as the checker sees it, which
    is what the test compares against so that it needs neither Ruby nor a
    database of its own.

Building the reference runs Mastodon's `schema.rb` through the real ActiveRecord
schema DSL. That needs Ruby, but not Mastodon: `activerecord` and `pg`, not its
thousand-gem bundle.

### Adopting a release

1.  `mise run mastodon:plan --to vX.Y.Z` lists the Rails migrations upstream
    added since the tracked release, and the deliberate divergences that now
    need re-examining against it.

2.  Write one eunha migration reproducing them, ending with an
    `INSERT INTO public.schema_migrations` of the versions it covers — a
    migration eunha deliberately does not implement is left out of that list, so
    that a Mastodon booted on the database still runs it. `--sql` prints the
    insert.

3.  Update `mastodon.toml` and `Cargo.toml`, replace `mastodon/schema.rb` with
    that release's, and run `mise run schema:build-reference`. The reference's
    diff is the schema delta you are adopting.

4.  Re-examine each divergence and move its `reviewed_for` forward in
    `divergences.toml`; the suite fails until every entry has been looked at.

5.  `mise run schema:check` against a database that has run the new migration.

6.  Rehearse it against real data before deploying. An instance gets one attempt
    at a migration:

    ~~~~
    scripts/rehearse_migration.sh postgres://user@localhost/seoul_earth
    ~~~~

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

~~~~
ACTIVE_RECORD_ENCRYPTION__PRIMARY_KEY=...
ACTIVE_RECORD_ENCRYPTION__KEY_DERIVATION_SALT=...
~~~~

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

### Update notices

Mastodon polls an update server for newer releases and for the end of support
of the branch it runs, and records both in `software_updates` and
`software_deprecations`. Eunha asks the same server the same question about the
Mastodon release *it implements*: eunha builds 4.7.1's schema and serves its
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
conditions. 4.7.1's instance serializer emits `icon` and `wrapstodon` that the
TypeScript does not mention, so the serializers are the authority on what
exists.

~~~~
mise run entities:build        # re-record from a Mastodon checkout
~~~~

That reads a clone at `~/Git/mastodon` (`MASTODON_REPO` to point elsewhere) at
the tracked tag rather than its working tree, fetching tags if the tag is
missing. Mastodon is not a submodule: 424MB of history for 468KB of files that
only matter when adopting a release, and a submodule bump's diff is a SHA,
whereas the diff of what is recorded here *is* the change being adopted.

### Differential testing against a live Mastodon

The entity check compares eunha to what upstream's serializers *say*. This one
asks upstream directly: the same request goes to both servers and the responses
are compared.

~~~~
scripts/differential_test.sh                              # its own eunha
scripts/differential_test.sh http://localhost:3001 TOKEN  # one you run
~~~~

Given no arguments it brings up an eunha of its own — scratch database,
migrations, two accounts and a token — and tears it down afterwards. That form
exists so CI and a developer run the same path: the eunha-side setup used to
live in whoever had last run it, which is most of why this went weeks without
being run at all. It needs `target/release/eunha` built, as
`federation_test.sh` does.

That brings up Mastodon in Docker — the official image, because building it from
source on macOS means libidn, OpenSSL headers for `hiredis-client`, libvips, and
a `pg` gem that segfaults against Postgres 18, all of which the image has
already solved — mints tokens, and compares what a client actually does: 31
reads, nine writes, and the interaction verbs (favourite, boost, bookmark, pin,
follow, block, mute, and their undos).

The stack runs Sidekiq as well as the web process. Without a worker nothing
Mastodon defers ever happens, and some of that shows in the API — a home feed
stays `regenerating?` and answers 206 forever — which reads as a difference from
eunha when it is a missing worker. An unfaithful reference invents findings.

It invented ten. Sidekiq boots as soon as Postgres answers, but the schema is
created by the web process's `db:prepare`, so on a cold database the worker
started first, died on `relation "users" does not exist`, and compose did not
bring it back. `unfavourite` and `unreblog` hand the removal to a worker and
force the flag false in *their own* response, so each undo looked right while
the row survived — and every later request read that row and reported
`favourited: true` on a status that had just been unfavourited. Nine
`favourited` differences and one `reblogged`, all recorded against eunha, all of
them a dead worker. The worker now restarts until the schema exists, and the
harness asks Redis whether one has registered before it compares anything,
rather than trusting that a container was started.

Each do/undo pair also acts on a status of its own now. Sharing one across all
ten verbs meant one pair's deferred work was visible to the next, and `unreblog`
opens a window Mastodon disagrees with itself in: `Status.reblogs_map` is
`unscoped` and counts the discarded reblog, while `Account#reblogged?` goes
through `default_scope { recent.kept }` and does not, so two endpoints answer
differently about the same status depending on whether the controller passes a
relationships presenter. A pair per status leaves nothing to leak.

Nothing in it encodes what the answer should be, which is the point: a rule
misread while writing a test would be misread in the test too. It found seven
differences the source-reading had missed, all of them fields nested inside
objects the entity extraction never descended into.

It compares values on writes and interactions, where both servers act on the
same input so a count or a flag that differs is a real difference — that is
where `poll.voted` was `false` for a poll's own author, `noindex` told every
account to hide from search engines, and a status came back with no language at
all. Identifiers, hostnames, timestamps and totals over an instance's whole
history are excluded, because two servers cannot agree on those however
identical the request, and comparing them buries everything else.

For reads it compares values only under `configuration.*` — the limits an
instance states about itself, where two servers genuinely should agree. That
came second, after a shape-only comparison passed `max_display_name_length: 30`
against Mastodon's 40, both being integers. Comparing values found the media
description limit still advertised at Mastodon's older 1500 rather than 10,000.

Everything else stays shape-only on purpose. `followers_count`, ids and
timestamps depend on each instance's data, and comparing them would bury real
findings under differences that mean nothing.

It runs in CI, as its own job, for the reason everything else here does: the two
ways it broke — a reference with no worker, and a compose file another harness
had edited out from under it — were both invisible to anyone not running it.

One comparison is built rather than observed. **Notification grouping** is the
part of that API which is not a straight translation of a row: Mastodon
collapses notifications into groups, and a client renders “X and 2 others
favourited your post” out of a group's `notifications_count` and
`sample_account_ids`. A server that groups differently shows a different
sentence with every field present and of the right type, so shape cannot see it
— and neither can one account, because a group of one is a group on any server.
Three further accounts favourite the same status and follow the same account,
and the groups are compared. Account ids cannot match between two servers, so
the samples are compared by *who* they name: each fan is known by the position
it acts in, and a group naming `[fan3, fan2, fan1]` here has to name
`[fan3, fan2, fan1]` there. It agrees, on both the count and the order.

Getting there needed the fixture reset on both sides, and the second reset is
the one worth remembering: eunha gets a scratch database every run while the
Mastodon container is left up between them, so clearing the notifications is not
enough. A repeat follow produces no notification at all, so on a second run only
the server with a fresh database reports a follow group — which reads exactly
like eunha inventing one. The fans unfollow before they follow.

### Federating with a live Mastodon

The differential harness asks whether the two servers answer a client the same
way. This one asks whether they can talk to *each other*: eunha's other
federation tests run eunha against eunha, where both sides share eunha's reading
of ActivityPub, so a misreading is invisible. This builds a pair that shares
nothing but the specification.

~~~~
scripts/federation_test.sh
scripts/federation_test.sh --keep      # leave both up to poke at
~~~~

Twenty-four checks, both directions: resolve the account, follow, deliver a
status, favourite it, boost it, delete it, and see each land on the other side.

**Both servers run inside the container network.** eunha used to run on the host
behind a host-side Caddy, and that cannot work: `mastodon.test` is a network
alias, so a host process cannot resolve it — eunha could not fetch an actor's
public key and rejected every inbound activity with a 401. In the network they
resolve each other by name, and the harness needs no `/etc/hosts` entry, no
certificate trusted on the host, and no eunha built for this machine. The
certificates are a throwaway CA made with `openssl`, trusted by both sides
through `SSL_CERT_FILE` and by nothing else; the script drives both servers over
plain published ports, which is why nothing on the host has to trust them.

Two things that cost a while, both recorded in the script:

 -  **Rails refuses a request whose `Host` is not its `LOCAL_DOMAIN`** — 403 on
    every endpoint, including those needing no authentication. Since
    `mastodon.test` does not resolve on the host, the only way in is the
    published port with the name supplied by hand. Without it the readiness
    loop spins forever against a Mastodon that is up and answering.
 -  **A follow commits at different moments on the two sides.** The sender
    listing the receiver as a follower is what makes it *deliver*; the receiver
    having committed the follow is what makes it *keep* what arrives, because
    Mastodon drops an activity from an account no local account follows yet.
    Waiting on the sender alone leaves a gap in which a status is delivered,
    accepted with a 2xx, and silently discarded — one run missed by 2.9ms, and
    it read as eunha failing to deliver.

The run also exercises what nothing else did: eunha delivers to Mastodon's
**shared inbox** (`https://mastodon.test/inbox`), not its per-actor one.
Mastodon delivers to eunha's per-actor inbox, which is its own choice when there
is a single recipient on the far side.

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

At the time of writing there are seven, covering integrity proofs on outgoing
activities, the invite tree and the two ways eunha's invite API goes beyond
Mastodon's, what the update check asks about, when the local-keypair migration
is recorded, and what a mute silences. Read the file rather than this paragraph:
the file is the one that has to stay true.

### Outstanding from 4.7.1

Nothing. Signing HTTP Message Signatures with Ed25519 or ML-DSA keys remains
unimplemented, but so is it in Mastodon: local accounts' HTTP signatures are
RSA on both sides. Both algorithms are verified inbound.

4.7.1 changed no schema — `db/schema.rb` is byte-identical to 4.7.0's, and the
two migrations it touched only make an interrupted `CREATE INDEX CONCURRENTLY`
re-runnable, which eunha's migration 008 avoids by building that index inside
its transaction. Nor did it change a serializer, so `entities.json` is
unchanged. It is three security fixes and five bug fixes, and none of them
lands on code eunha has:

 -  **Password bypass in 2FA for LDAP/PAM/SSO accounts**
    ([GHSA-vx32-x96w-qq65]). `external_or_valid_password?` treated a blank
    `encrypted_password` as proof, so anyone could pass the first factor for an
    externally-authenticated account. Eunha authenticates against
    `encrypted_password` alone — no LDAP, no PAM, no SSO — and a blank hash
    parses as neither bcrypt nor argon2, so it fails rather than passes.

 -  **Denial of service on pathological JSON-LD** ([GHSA-vgm8-frgh-rh2v]).
    Upstream compacts a document carrying a `signature` before deciding whether
    to trust it, and the JSON-LD processor can be made to do unbounded work.
    Eunha does no JSON-LD expansion or compaction at all and processes no
    LD signature; what it reads of an inbound body is bounded by axum's 2MB
    default on the ActivityPub routes and serde\_json's 128-deep recursion
    limit.

 -  **Disabled staff keeping admin API access** ([GHSA-62j4-hvj7-px3f]). The
    admin REST controllers never ran the permission check the web UI did, and
    the policy's `role` did not consider `disabled`. Eunha rejects a disabled
    user's token in `middleware::authenticate`, before routing — the account has
    no API access of any kind, not merely no admin API.

The rest are a Rails admin-UI form parameter, a Dockerfile Bootsnap path, and
the `mastodon:setup` rake task, none of which eunha has. The one behavioural fix
that touches something eunha does — `invite_text_required?` no longer treating
any invite as reason enough to skip the invite text — lands on
`Setting.require_invite_text`, which eunha does not implement.

[GHSA-vx32-x96w-qq65]: https://github.com/mastodon/mastodon/security/advisories/GHSA-vx32-x96w-qq65
[GHSA-vgm8-frgh-rh2v]: https://github.com/mastodon/mastodon/security/advisories/GHSA-vgm8-frgh-rh2v
[GHSA-62j4-hvj7-px3f]: https://github.com/mastodon/mastodon/security/advisories/GHSA-62j4-hvj7-px3f


Invites
-------

Who may invite is Mastodon's `invite_users` permission, and it lives on the
**everyone role** — `user_roles` id -99, seeded by migration 009 with
`UserRole::Flags::DEFAULT`, which is that one permission. Every member has that
role unless given another, so an instance invites the way upstream does until it
says otherwise. One that would rather hand invites out itself takes the bit off:

~~~~
UPDATE user_roles SET permissions = permissions & ~(1 << 16) WHERE id = -99;
~~~~

Staff keep it through their own role. A role carrying `administrator` (1 << 0)
computes to every permission there is, and any other role's permissions are
unioned with the everyone role's — Mastodon's `UserRole#computed_permissions`,
which is why upstream's Admin role does not list `invite_users` and an admin can
still invite. `verify_credentials` reports that computed set rather than the raw
column, so a client hides the invite page for exactly the accounts the server
would refuse.

Eunha has no role editor and no `eunha` subcommand for one: roles are rows, and
an instance that needs to change one runs the `UPDATE`. Upstream will only let
the everyone role hold `Flags::SAFE` — `invite_users` and
`invite_bypass_approval` — and nothing here enforces that, so a hand-written
value should stay inside it.

The other half of `SAFE` is what an invite does to the approval queue. On an
instance with `approval_required`, an invite skips review only when whoever
wrote it holds `invite_bypass_approval`, which is Mastodon's
`Invite#bypass_approval?` — a question about the inviter, not about whether an
invite was used. The everyone role does not carry it, so an ordinary member may
bring someone and the admin still sees them first. An instance that would rather
an invite be the whole of the decision grants it:

~~~~
UPDATE user_roles SET permissions = permissions | (1 << 21) WHERE id = -99;
~~~~

Staff invites bypass already, through the `administrator` flag.

### Handing them out

That leaves an instance where only staff may invite, which on its own would
flatten the invite tree: every arrival would be a child of the admin rather than
of whoever actually brought them. So an admin can mint codes **into a member's
own account** instead — `POST /api/eunha/v1/invite_grants`, and the “Hand out
invites” panel on the invite page, for one member or for the whole userbase at
once. They appear on that member's page to copy and pass on, and a signup
through one lands under them.

The count is the limit; there is no allowance to keep books on, because the
codes themselves are the allowance. `manage_invites` is what it takes to hand
them out, and listing your own invites takes no permission at all — a member who
cannot create one still has to be able to read what they were given, which is
where eunha parts company with `InvitesController#index`.
