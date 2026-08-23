#!/usr/bin/env python3
"""Compare eunha's API responses against a live Mastodon's.

The other checks in this repo compare eunha to a *reading* of Mastodon — its
serializers, its callbacks, its scopes. This one asks Mastodon itself: the same
request goes to both servers, and the two responses are compared field by field.
A rule misread while writing a test is a rule wrong in the test too; this cannot
make that mistake, because nothing here encodes what the answer should be.

What is compared is shape and kind, not content: ids, hostnames, timestamps and
tokens necessarily differ between two servers, so a field present on both with
the same JSON type agrees. What it catches is a field one server sends and the
other does not, a field whose type differs, and a status code that differs —
which is where every entity-level bug found this session lived.

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


def request(base, path, token, method="GET"):
    """Returns (status, parsed_body_or_None, header_names)."""
    req = urllib.request.Request(f"{base}{path}", method=method)
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


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--eunha", required=True)
    parser.add_argument("--mastodon", required=True)
    parser.add_argument("--eunha-token", required=True)
    parser.add_argument("--mastodon-token", required=True)
    parser.add_argument("--only", help="compare just paths containing this")
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
