#!/usr/bin/env python3
"""Compare eunha's API responses against a live Mastodon's.

The other checks in this repo compare eunha to a *reading* of Mastodon — its
serializers, its callbacks, its scopes. This one asks Mastodon itself: the same
request goes to both servers, and the two responses are compared field by field.
A rule misread while writing a test is a rule wrong in the test too; this cannot
make that mistake, because nothing here encodes what the answer should be.

Two things are compared. **Shape** — which fields exist and what kind each holds
— everywhere, because ids, hostnames, timestamps and tokens necessarily differ
between two servers and asking them to agree would drown the signal. And
**values**, but only for fields where two servers genuinely should agree: the
constants and limits an instance advertises about itself, listed in
`COMPARED_VALUES` below.

That second part exists because the first was not enough. eunha advertised
`max_display_name_length: 30` where Mastodon says 40 — so a client would have
refused a display name this server accepts — and a shape comparison passed it,
both being integers.

Usage:
    scripts/differential_test.py --eunha http://localhost:3001 \
                                 --mastodon http://localhost:3000 \
                                 --eunha-token TOKEN --mastodon-token TOKEN
"""
import argparse
import json
import sys
import urllib.error
import urllib.request

# Endpoints worth comparing: what a client touches on an ordinary session.
# Each is (method, path, needs_auth).
ENDPOINTS = [
    ("GET", "/api/v1/instance", False),
    ("GET", "/api/v2/instance", False),
    ("GET", "/api/v1/instance/rules", False),
    ("GET", "/api/v1/instance/peers", False),
    ("GET", "/api/v1/custom_emojis", False),
    ("GET", "/api/v1/accounts/verify_credentials", True),
    ("GET", "/api/v1/preferences", True),
    ("GET", "/api/v1/filters", True),
    ("GET", "/api/v2/filters", True),
    ("GET", "/api/v1/lists", True),
    ("GET", "/api/v1/markers?timeline[]=home", True),
    ("GET", "/api/v1/notifications", True),
    ("GET", "/api/v1/timelines/home", True),
    ("GET", "/api/v1/timelines/public", False),
    ("GET", "/api/v1/conversations", True),
    ("GET", "/api/v1/bookmarks", True),
    ("GET", "/api/v1/favourites", True),
    ("GET", "/api/v1/follow_requests", True),
    ("GET", "/api/v1/mutes", True),
    ("GET", "/api/v1/blocks", True),
    ("GET", "/api/v1/domain_blocks", True),
    ("GET", "/api/v1/endorsements", True),
    ("GET", "/api/v1/featured_tags", True),
    ("GET", "/api/v1/suggestions", True),
    ("GET", "/api/v2/suggestions", True),
    ("GET", "/api/v1/trends/tags", False),
    ("GET", "/api/v1/trends/statuses", False),
    ("GET", "/api/v1/trends/links", False),
    ("GET", "/api/v1/announcements", True),
    ("GET", "/api/v1/notifications/policy", True),
    ("GET", "/api/v1/scheduled_statuses", True),
]


def request(base, path, token, method="GET", body=None):
    """Returns (status, parsed_body_or_None, header_names)."""
    data = None
    if body is not None:
        data = json.dumps(body).encode()
    req = urllib.request.Request(f"{base}{path}", method=method, data=data)
    if data is not None:
        req.add_header("Content-Type", "application/json")
    if token:
        req.add_header("Authorization", f"Bearer {token}")
    req.add_header("Accept", "application/json")
    # Mastodon's production environment sets `config.force_ssl`, and would
    # answer 301 to a plain request. This is what a reverse proxy in front of it
    # would send, and is how it is actually deployed.
    req.add_header("X-Forwarded-Proto", "https")
    try:
        with urllib.request.urlopen(req, timeout=30) as response:
            raw = response.read()
            status, headers = response.status, dict(response.headers)
    except urllib.error.HTTPError as e:
        raw, status, headers = e.read(), e.code, dict(e.headers)
    except Exception as e:
        return None, {"__transport_error__": str(e)}, {}
    try:
        return status, json.loads(raw), headers
    except json.JSONDecodeError:
        return status, {"__not_json__": raw[:200].decode("utf-8", "replace")}, headers


# Fields whose value two servers should agree on: limits and constants an
# instance states about itself, rather than anything derived from its data.
# Matched against the dotted path a field sits at, so `configuration.accounts.
# max_note_length` is compared and `accounts.note` is not.
COMPARED_VALUES = (
    "configuration.accounts.",
    "configuration.statuses.",
    "configuration.polls.",
    "configuration.media_attachments.",
    "configuration.translation.",
    "configuration.reactions.",
)


def compares_value(path):
    return any(path.startswith(prefix) for prefix in COMPARED_VALUES)


def values_at(value, prefix="", out=None):
    """Flatten a response to {dotted.path: scalar} for the fields we compare."""
    out = {} if out is None else out
    if isinstance(value, dict):
        for k, v in value.items():
            values_at(v, f"{prefix}.{k}" if prefix else k, out)
    elif not isinstance(value, list):
        if compares_value(prefix):
            out[prefix] = value
    return out


def shape(value, depth=0):
    """A value's structure, ignoring content.

    Two servers cannot agree on ids, hostnames or timestamps, and should not be
    asked to. They can agree on which fields exist and what kind each holds.
    """
    if depth > 6:
        return "..."
    if isinstance(value, dict):
        return {k: shape(v, depth + 1) for k, v in sorted(value.items())}
    if isinstance(value, list):
        # A list's shape is its first element's: an empty list on one side says
        # nothing about disagreement, only that the fixture differed.
        return [shape(value[0], depth + 1)] if value else []
    if value is None:
        return "null"
    return type(value).__name__


def compare(path, left, right, findings, where=""):
    """Walk two shapes together, recording where they differ."""
    if isinstance(left, dict) and isinstance(right, dict):
        for key in sorted(set(left) | set(right)):
            at = f"{where}.{key}" if where else key
            if key not in right:
                findings.append(f"{path}: eunha sends `{at}`, Mastodon does not")
            elif key not in left:
                findings.append(f"{path}: Mastodon sends `{at}`, eunha does not")
            else:
                compare(path, left[key], right[key], findings, at)
    elif isinstance(left, list) and isinstance(right, list):
        if left and right:
            compare(path, left[0], right[0], findings, f"{where}[]")
    elif left != right:
        # `null` against a type is not a disagreement: a nullable field simply
        # held a value on one server and not the other.
        if "null" not in (left, right):
            findings.append(f"{path}: `{where}` is {left} on eunha, {right} on Mastodon")


# Things a client does, rather than reads. Each is (method, path, body, name);
# the response entity is compared like any other, and the status code with it.
#
# These matter more than the reads: a GET returns what the server already holds,
# while a POST is the server deciding what to make of a request. eunha and
# Mastodon can agree on every timeline and still disagree on what posting a
# status with a poll produces.
WRITES = [
    ("POST", "/api/v1/statuses", {"status": "a plain status"}, "post a status"),
    (
        "POST",
        "/api/v1/statuses",
        {"status": "with a spoiler", "spoiler_text": "cw", "sensitive": True},
        "post behind a content warning",
    ),
    (
        "POST",
        "/api/v1/statuses",
        {"status": "unlisted please", "visibility": "unlisted"},
        "post unlisted",
    ),
    (
        "POST",
        "/api/v1/statuses",
        {"status": "which?", "poll": {"options": ["a", "b"], "expires_in": 3600}},
        "post a poll",
    ),
    ("POST", "/api/v1/lists", {"title": "a list"}, "create a list"),
    (
        "POST",
        "/api/v2/filters",
        {"title": "a filter", "context": ["home"], "filter_action": "warn"},
        "create a filter",
    ),
    # Rejections are part of the contract too, and are where two servers most
    # easily differ: the same bad request should fail the same way.
    ("POST", "/api/v1/statuses", {"status": ""}, "reject an empty status"),
    (
        "POST",
        "/api/v1/statuses",
        {"status": "x", "visibility": "nonsense"},
        "reject an unknown visibility",
    ),
    ("POST", "/api/v1/lists", {}, "reject a list with no title"),
]


def compare_writes(args, findings):
    """Send each write to both servers and compare what comes back."""
    compared = 0
    for method, path, body, name in WRITES:
        e_status, e_body, _ = request(args.eunha, path, args.eunha_token, method, body)
        m_status, m_body, _ = request(args.mastodon, path, args.mastodon_token, method, body)

        if e_status is None or m_status is None:
            continue
        compared += 1
        if e_status != m_status:
            findings.append(f"{name}: eunha {e_status}, Mastodon {m_status}")
            continue
        # A rejection's body is a message, and two servers word those
        # differently; the status code is the part a client acts on.
        if m_status < 400:
            compare(name, shape(e_body), shape(m_body), findings)
    return compared


# Interactions need something to act on, and the two servers cannot share ids.
# So each is given its own status and its own second account, and the *responses*
# are compared — favouriting your own post should produce the same entity here
# as there, whatever the ids inside it are.
def compare_interactions(args, findings):
    """Post, then act on what was posted, comparing each response."""
    compared = 0

    def on(server, token, method, path, body=None):
        return request(server, path, token, method, body)

    servers = [
        ("eunha", args.eunha, args.eunha_token, args.eunha_other_id),
        ("mastodon", args.mastodon, args.mastodon_token, args.mastodon_other_id),
    ]

    # A status on each, to act upon.
    posted = {}
    for name, base, token, _ in servers:
        status, body, _ = on(base, token, "POST", "/api/v1/statuses",
                             {"status": "something to react to"})
        if status != 200:
            findings.append(f"interactions: {name} would not accept a status ({status})")
            return compared
        posted[name] = body["id"]

    # (verb, path template, name) — each acts on that server's own status.
    status_verbs = [
        ("favourite", "/api/v1/statuses/{id}/favourite"),
        ("unfavourite", "/api/v1/statuses/{id}/unfavourite"),
        ("reblog", "/api/v1/statuses/{id}/reblog"),
        ("unreblog", "/api/v1/statuses/{id}/unreblog"),
        ("bookmark", "/api/v1/statuses/{id}/bookmark"),
        ("unbookmark", "/api/v1/statuses/{id}/unbookmark"),
        ("pin", "/api/v1/statuses/{id}/pin"),
        ("unpin", "/api/v1/statuses/{id}/unpin"),
        ("mute conversation", "/api/v1/statuses/{id}/mute"),
        ("unmute conversation", "/api/v1/statuses/{id}/unmute"),
    ]
    for verb, template in status_verbs:
        results = {}
        for name, base, token, _ in servers:
            results[name] = on(base, token, "POST", template.format(id=posted[name]))
        compared += 1
        e_status, e_body, _ = results["eunha"]
        m_status, m_body, _ = results["mastodon"]
        if e_status != m_status:
            findings.append(f"{verb}: eunha {e_status}, Mastodon {m_status}")
        elif m_status < 400:
            compare(verb, shape(e_body), shape(m_body), findings)

    # And the verbs that act on an account.
    account_verbs = [
        ("follow", "/api/v1/accounts/{id}/follow"),
        ("mute", "/api/v1/accounts/{id}/mute"),
        ("unmute", "/api/v1/accounts/{id}/unmute"),
        ("block", "/api/v1/accounts/{id}/block"),
        ("unblock", "/api/v1/accounts/{id}/unblock"),
        ("unfollow", "/api/v1/accounts/{id}/unfollow"),
    ]
    for verb, template in account_verbs:
        results = {}
        for name, base, token, other in servers:
            if not other:
                results = {}
                break
            results[name] = on(base, token, "POST", template.format(id=other))
        if not results:
            continue
        compared += 1
        e_status, e_body, _ = results["eunha"]
        m_status, m_body, _ = results["mastodon"]
        if e_status != m_status:
            findings.append(f"{verb}: eunha {e_status}, Mastodon {m_status}")
        elif m_status < 400:
            compare(verb, shape(e_body), shape(m_body), findings)

    return compared


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--eunha", required=True)
    parser.add_argument("--mastodon", required=True)
    parser.add_argument("--eunha-token", required=True)
    parser.add_argument("--mastodon-token", required=True)
    parser.add_argument("--only", help="compare just paths containing this")
    parser.add_argument("--eunha-other-id", help="a second account on eunha, to follow and block")
    parser.add_argument("--mastodon-other-id", help="the same, on Mastodon")
    args = parser.parse_args()

    findings, compared, skipped = [], 0, []
    for method, path, needs_auth in ENDPOINTS:
        if args.only and args.only not in path:
            continue
        e_status, e_body, _ = request(
            args.eunha, path, args.eunha_token if needs_auth else None, method
        )
        m_status, m_body, _ = request(
            args.mastodon, path, args.mastodon_token if needs_auth else None, method
        )

        if e_status is None or m_status is None:
            skipped.append(f"{path}: transport error")
            continue
        # An endpoint Mastodon does not implement at this version is not drift.
        if m_status == 404 and e_status == 404:
            continue
        if e_status != m_status:
            findings.append(f"{path}: eunha {e_status}, Mastodon {m_status}")
            continue
        if m_status >= 400:
            skipped.append(f"{path}: both {m_status}")
            continue

        compared += 1
        compare(path, shape(e_body), shape(m_body), findings)

        # And the values that are not a matter of instance data.
        e_values, m_values = values_at(e_body), values_at(m_body)
        for field in sorted(set(e_values) & set(m_values)):
            if e_values[field] != m_values[field]:
                findings.append(
                    f"{path}: `{field}` is {e_values[field]!r} on eunha, "
                    f"{m_values[field]!r} on Mastodon"
                )

    if not args.only:
        compared += compare_writes(args, findings)
        compared += compare_interactions(args, findings)

    print(f"compared {compared} endpoint(s)")
    for s in skipped:
        print(f"  skipped {s}")
    if not findings:
        print("\nNo differences.")
        return 0
    print(f"\n{len(findings)} difference(s):")
    for f in findings:
        print(f"  {f}")
    return 1


if __name__ == "__main__":
    sys.exit(main())
