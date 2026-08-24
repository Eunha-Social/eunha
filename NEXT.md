Where to pick this up
=====================

State at handoff: `main` is deployed to seoul.earth and healthy. Schema matches
Mastodon 4.7.0 across 974 constraints. 856 integration tests, 70 unit tests, CI
green on seven gates — the differential harness is one of them now, and runs
clean against a live Mastodon 4.7.0.

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

**eunha returns 503 for `/ap/users/:id/collections/featured`.** Seen while
Mastodon fetched it during federation. Harmless to the handshake and never
chased down.

**Actor resolution over the wire is exercised; `sharedInbox` delivery is not.**
Mastodon prefers the shared inbox and eunha advertises one, but the federation
harness only ever drives per-actor inboxes.


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
4.  **Give `federation_test.sh` the worker gate too.** It has the same boot race
    — now fixed in its compose file — and depends on Sidekiq for every delivery,
    so a dead worker there reports eunha as unreachable. The gate is eight lines
    in `differential_test.sh`; it was not copied across because the federation
    harness could not be run from here to prove the change.


How not to be fooled
--------------------

Five failures in this work were the tooling rather than eunha, and each cost
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

The general form: a passing check is not evidence until you have seen it fail.
Disable the fix and confirm the test notices.
