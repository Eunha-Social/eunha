#!/usr/bin/env python3
"""Federate eunha with a real Mastodon, in both directions, and check what lands.

The rest of this repo's federation tests run eunha against eunha. Both sides
then share eunha's reading of ActivityPub, so a misreading is invisible: two
servers agreeing about something they are both wrong about looks exactly like
correctness. This drives the same activities between eunha and Mastodon 4.7.0,
where nothing is shared but the specification.

Each case makes something happen through one server's own API — so the activity
is signed, addressed and delivered by that server's real code — and then asks
the other server what it received. Delivery is asynchronous on both sides, so
every check polls.

Run it through scripts/federation_test.sh, which builds the pair.

Known state, as of writing: Mastodon → eunha passes every check — follow,
status, favourite, boost and delete all cross and are understood. eunha →
Mastodon establishes follows in both directions, but a status posted on eunha
does not appear on Mastodon, while eunha's log shows it delivering. That last
one is a real lead rather than an environment problem, and is where to pick this
up.

Five things about the environment took a while to find, and all five make a
correct implementation look broken:

* **Port 443 or nothing.** Mastodon webfingers an account by the *host* of its
  actor URI and drops the port, so eunha on `:3002` is looked up on `:443` and
  every delivery fails verification. eunha sits behind a proxy on 443 for this
  reason, not by preference.
* **A private address is refused by both.** Mastodon has `ALLOWED_PRIVATE_
  ADDRESSES` for it; eunha's `PublicOnlyResolver` has no equivalent, so remote
  actors are seeded rather than fetched.
* **The image runs as a non-root user**, so the CA goes in through
  `SSL_CERT_FILE` — appended to the image's own bundle, not replacing it.
* **No Sidekiq means nothing is delivered at all**, silently.
* **`ALLOWED_PRIVATE_ADDRESSES` belongs on the sidekiq service, not just web.**
  Deliveries run in sidekiq; without it every one fails before a request is
  made, and Mastodon's circuit breaker then trips the inbox to red and stops
  trying. The symptom is no request, no error, and empty queues — which reads as
  "Mastodon silently refuses to talk to eunha" and is nothing of the kind. If
  deliveries stop arriving, look for `stoplight:` keys in Mastodon's Redis
  before suspecting eunha.
"""
import argparse
import json
import sys
import time
import urllib.error
import urllib.request


class Server:
    """One side of the pair, driven through its client API."""

    def __init__(self, name, base, token, extra_headers=None):
        self.name = name
        self.base = base.rstrip("/")
        self.token = token
        self.extra_headers = extra_headers or {}

    def call(self, method, path, body=None, token=None):
        data = json.dumps(body).encode() if body is not None else None
        req = urllib.request.Request(f"{self.base}{path}", method=method, data=data)
        req.add_header("Authorization", f"Bearer {token or self.token}")
        req.add_header("Accept", "application/json")
        if data is not None:
            req.add_header("Content-Type", "application/json")
        for k, v in self.extra_headers.items():
            req.add_header(k, v)
        try:
            with urllib.request.urlopen(req, timeout=30) as response:
                raw = response.read()
                return response.status, (json.loads(raw) if raw else None)
        except urllib.error.HTTPError as e:
            raw = e.read()
            try:
                return e.code, json.loads(raw)
            except json.JSONDecodeError:
                return e.code, {"raw": raw[:200].decode("utf-8", "replace")}
        except Exception as e:
            return None, {"transport_error": str(e)}


def until(predicate, seconds=25, interval=1.0):
    """Poll, because delivery is a queue on both sides, not a function call."""
    deadline = time.time() + seconds
    last = None
    while time.time() < deadline:
        last = predicate()
        if last:
            return last
        time.sleep(interval)
    return last


class Report:
    def __init__(self):
        self.results = []

    def check(self, direction, name, ok, detail=""):
        self.results.append((direction, name, bool(ok), detail))
        mark = "ok  " if ok else "FAIL"
        line = f"  [{mark}] {direction}: {name}"
        print(f"{line}    {detail}" if detail and not ok else line)

    def failed(self):
        return [r for r in self.results if not r[2]]


def find_account(server, acct):
    """Look up an account by its full handle, resolving it if unseen."""
    status, body = server.call("GET", f"/api/v1/accounts/lookup?acct={acct}")
    if status == 200 and body:
        return body
    status, body = server.call(
        "GET", f"/api/v2/search?q={acct}&type=accounts&resolve=true"
    )
    if status == 200 and body and body.get("accounts"):
        return body["accounts"][0]
    return None


def run_direction(sender, receiver, sender_acct, receiver_acct, report):
    """Drive sender → receiver: follow, post, favourite, boost, delete."""
    direction = f"{sender.name}→{receiver.name}"

    # The receiving account as the sender sees it. Everything else needs this.
    remote = until(lambda: find_account(sender, receiver_acct), seconds=30)
    if not remote:
        report.check(direction, "resolve the remote account", False,
                     f"{sender.name} could not find {receiver_acct}")
        return
    report.check(direction, "resolve the remote account", True)

    # Start from no relationship, so a run says what this run did rather than
    # what a previous one left behind. Both sides, since either may hold one.
    sender.call("POST", f"/api/v1/accounts/{remote['id']}/unfollow")
    back0 = find_account(receiver, sender_acct)
    if back0:
        receiver.call("POST", f"/api/v1/accounts/{back0['id']}/unfollow")
    time.sleep(3)

    # ── Follow ──────────────────────────────────────────────────────────────
    status, _ = sender.call("POST", f"/api/v1/accounts/{remote['id']}/follow")
    ok = status == 200
    report.check(direction, "follow is accepted locally", ok, f"status {status}")

    def followed():
        st, body = receiver.call("GET", "/api/v1/accounts/verify_credentials")
        if st != 200:
            return False
        st, followers = receiver.call(
            "GET", f"/api/v1/accounts/{body['id']}/followers"
        )
        return st == 200 and any(
            a.get("acct") == sender_acct for a in (followers or [])
        )

    report.check(direction, "follow arrives", bool(until(followed)))

    # A status is delivered to the author's followers, so the receiver has to
    # follow the sender for anything to arrive. That is a Follow in the other
    # direction — tested on its own in the other pass; here it is setup.
    back = until(lambda: find_account(receiver, sender_acct), seconds=30)
    if not back:
        report.check(direction, "receiver can resolve the sender", False,
                     f"{receiver.name} could not find {sender_acct}")
        return
    st, _ = receiver.call("POST", f"/api/v1/accounts/{back['id']}/follow")
    if st != 200:
        report.check(direction, "receiver follows the sender", False,
                     f"{receiver.name} refused the follow: status {st}")
        return

    def follows_back():
        st, body = sender.call("GET", "/api/v1/accounts/verify_credentials")
        if st != 200:
            return False
        st, followers = sender.call("GET", f"/api/v1/accounts/{body['id']}/followers")
        return st == 200 and any(
            a.get("acct") == receiver_acct for a in (followers or [])
        )

    if not until(follows_back):
        report.check(direction, "receiver follows the sender", False,
                     "without this a status has nowhere to be delivered")
        return
    report.check(direction, "receiver follows the sender", True)

    # ── Create ──────────────────────────────────────────────────────────────
    marker = f"federated-{int(time.time() * 1000)}"
    status, posted = sender.call(
        "POST", "/api/v1/statuses", {"status": f"hello {marker}", "visibility": "public"}
    )
    if status != 200:
        report.check(direction, "post a status", False, f"status {status}")
        return
    report.check(direction, "post a status", True)

    def received_status():
        st, body = receiver.call("GET", f"/api/v2/search?q={marker}&resolve=false")
        if st != 200 or not body:
            return None
        for s in body.get("statuses", []):
            if marker in (s.get("content") or ""):
                return s
        return None

    arrived = until(received_status, seconds=30)
    report.check(direction, "status arrives", bool(arrived),
                 "not found on the other side")
    if not arrived:
        return

    # ── Like ────────────────────────────────────────────────────────────────
    status, _ = receiver.call("POST", f"/api/v1/statuses/{arrived['id']}/favourite")
    report.check(direction, "favourite is accepted", status == 200, f"status {status}")

    def favourited():
        st, body = sender.call("GET", f"/api/v1/statuses/{posted['id']}")
        return st == 200 and (body or {}).get("favourites_count", 0) >= 1

    report.check(direction, "favourite comes back", bool(until(favourited)))

    # ── Announce ────────────────────────────────────────────────────────────
    status, _ = receiver.call("POST", f"/api/v1/statuses/{arrived['id']}/reblog")
    report.check(direction, "boost is accepted", status == 200, f"status {status}")

    def boosted():
        st, body = sender.call("GET", f"/api/v1/statuses/{posted['id']}")
        return st == 200 and (body or {}).get("reblogs_count", 0) >= 1

    report.check(direction, "boost comes back", bool(until(boosted)))

    # ── Delete ──────────────────────────────────────────────────────────────
    status, _ = sender.call("DELETE", f"/api/v1/statuses/{posted['id']}")
    report.check(direction, "delete is accepted locally", status == 200,
                 f"status {status}")

    def gone():
        st, body = receiver.call("GET", f"/api/v1/statuses/{arrived['id']}")
        return st in (404, 410)

    report.check(direction, "delete arrives", bool(until(gone)),
                 "the status is still there")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--eunha", required=True)
    parser.add_argument("--eunha-token", required=True)
    parser.add_argument("--eunha-acct", required=True,
                        help="the eunha account's full handle, e.g. alice@host:3002")
    parser.add_argument("--mastodon", required=True)
    parser.add_argument("--mastodon-token", required=True)
    parser.add_argument("--mastodon-acct", required=True)
    parser.add_argument("--only", choices=["to-eunha", "to-mastodon"])
    args = parser.parse_args()

    eunha = Server("eunha", args.eunha, args.eunha_token)
    mastodon = Server("mastodon", args.mastodon, args.mastodon_token)

    report = Report()
    if args.only != "to-mastodon":
        print("Mastodon → eunha")
        run_direction(mastodon, eunha, args.mastodon_acct, args.eunha_acct, report)
    if args.only != "to-eunha":
        print("\neunha → Mastodon")
        run_direction(eunha, mastodon, args.eunha_acct, args.mastodon_acct, report)

    failures = report.failed()
    print(f"\n{len(report.results) - len(failures)}/{len(report.results)} checks passed")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
