Where to pick this up
=====================

State at handoff: `main` is deployed to seoul.earth and healthy. Schema matches
Mastodon 4.7.0 across 974 constraints. 856 integration tests, 70 unit tests, CI
green on seven gates — the differential harness is one of them now, and runs
clean against a live Mastodon 4.7.0. Verified by looking at the run, not by
assuming: CI had in fact been red for at least two commits. The federation
harness passes 24/24 from a clean checkout, both servers inside the container
network.

Three harnesses verify parity, described in `README.md` and in each script's
header. Read those headers before believing a failure — most of what they report
first is the harness or the reference, not eunha.


What is unfinished
------------------

**Nothing the differential harness reports.** It runs clean: 56 endpoints, no
differences. The nine `favourited` findings were the harness, not eunha and not
the fixture — its Mastodon had no Sidekiq, so the rows `unfavourite` and
`unreblog` hand to a worker were never deleted and every later request read them
back. See `README.md`; the harness now refuses to compare without a live worker.

**`/api/v1/timelines/home` answering 206 on Mastodon and 200 on eunha** has not
been seen since. It was the same dead worker — a feed with nothing to regenerate
it stays `regenerating?` forever — so it may already be gone. If it comes back
with a worker running, it is real, and the harness should skip the comparison
while the feed rebuilds rather than report it.

**eunha returns 503 for `/ap/users/:id/collections/featured`** and for
`/ap/users/:id/collections`. Still reproduces on every federation run, while
Mastodon fetches them. Harmless to the handshake — 24/24 checks pass with it
happening — and never chased down.

**`sharedInbox` delivery is exercised now, and works.** eunha delivers to
`https://mastodon.test/inbox` rather than the per-actor inbox, and Mastodon
stores what arrives. Mastodon delivers to eunha's per-actor inbox — its own
choice when a single recipient is on the far side — so eunha's *inbound* shared
inbox is still the untested half.


Where bugs have actually been
-----------------------------

Thirty were found in the work leading here, and they clustered:

 -  **Guard clauses.** `return if direct_visibility?`,
    `if in_reply_to_id.present? && distributable?`, `return unless accepted?`.
    Unconditional logic was nearly always already right; the conditions are
    what a reimplementation drops. When reading upstream for a rule, read the
    first three lines of the method.
 -  **The ActivityPub ingest paths.** eunha's own API maintained counters that
    its inbox handlers did not — six bugs of one shape, because Mastodon gets
    this from model callbacks that fire however a record came to exist.
    `src/counters.rs` now owns those rules; keep new paths calling it rather
    than writing their own.
 -  **Values that are the right type.** `voted` false where Mastodon says true,
    `noindex` derived from an unrelated column, a limit advertised as 1500
    instead of 10000. A shape comparison passes all of these.


Things worth doing
------------------

1.  **Compare notification *grouping* end to end**, not just the key format. The
    rules are known and implemented; what is untested is a group's
    `notifications_count` and `sample_account_ids` against Mastodon's for the
    same fixture.
2.  **Run the harnesses against the deployed build**, not a local one. Nothing
    has ever checked that production matches what the tests claim.
3.  **`sharedInbox` delivery**, per above.
4.  **Put the federation harness in CI.** It is why it was moved into the
    container network: it now needs nothing the laptop has that a runner does
    not. The cost is a `docker build` of eunha on top of the Mastodon boot, so
    it is the slowest gate by some way — worth deciding whether it belongs on
    every pull request or on a schedule.


How not to be fooled
--------------------

Seven failures in this work were the tooling rather than eunha, and each cost
real time:

 -  A commit shipped tests without the definitions they needed, because a
    keyword heuristic quietly reverted work while separating it from `cargo fmt`
    churn. Run the suite *after* cleaning up, not before.
 -  The federation harness passed by hand for hours and failed from a clean
    checkout, because a scratch directory had accumulated fixes the repository
    never received. Tooling is not real until it runs from `git clone`.
 -  The same thing then happened in reverse, and is what hid the ten findings'
    real cause. The
    federation work grew TLS proxies onto the compose file the *differential*
    harness shared, mounting certificates generated into that scratch directory
    — so `differential_test.sh`, which runs its compose file in place, could no
    longer bring the stack up at all. The two now have a compose file each. A
    harness that another harness can break by editing is one harness.
 -  Ten differences were attributed to eunha for weeks because the reference was
    broken in a way that only ever made *eunha* look wrong. Before believing a
    finding, check that the thing you are comparing against is actually running.
 -  A deploy reported success having shipped nothing, because the push went to a
    branch that did not have the commits. Check the deployed *behaviour*, not
    the deploy's exit code — the 0.81s build was the tell.
 -  The federation harness had never worked from a clean checkout either, and
    for a reason no amount of re-reading would have found: eunha ran on the
    host, `mastodon.test` is a container-network alias, and a host process
    cannot resolve one. eunha could not fetch an actor's public key, so it
    rejected every inbound activity with a 401 — which reads as a signature bug.
    A commit message said it passed every check; what passed was a laptop with
    state nobody wrote down. Both servers live in the network now.
 -  This file claimed six green CI gates while CI had been red on `main` for at
    least two commits, because nobody had opened the Actions tab. The failures
    were *stacked*: CI ran `postgres:16`, which cannot execute
    `001_initial.sql` — its `pg_dump` preamble sets `transaction_timeout`, a
    parameter Postgres 17 introduced — so the first step died and hid a clippy
    error behind it. Development and production both run 18; CI was the only
    Postgres anywhere that was not. A red first gate means every gate after it
    has told you nothing.

The general form: a passing check is not evidence until you have seen it fail.
Disable the fix and confirm the test notices.
