#!/bin/bash
# Compare eunha against a live Mastodon, rather than against a reading of it.
#
# Every other check in this repo compares eunha to what upstream's source says.
# This one asks upstream: the same request goes to both servers and the two
# responses are compared. Nothing here encodes what the answer should be, so a
# rule misread while writing a test cannot pass here too.
#
# The official image is used deliberately. Building Mastodon from source on
# macOS means libidn, OpenSSL headers for hiredis-client, libvips, and a `pg`
# gem that segfaults against Postgres 18 — all of it already solved in the image.
#
# Needs a container runtime (OrbStack, Docker Desktop, colima) and eunha
# already running. Usage:
#   scripts/differential_test.sh http://localhost:3001 EUNHA_TOKEN
#
# Set EUNHA_OTHER_ID to a second account on the eunha side to include the
# interaction verbs — follow, block, mute — which otherwise compare nothing.
set -euo pipefail

EUNHA_URL="${1:?usage: differential_test.sh <eunha-url> <eunha-token>}"
EUNHA_TOKEN="${2:?usage: differential_test.sh <eunha-url> <eunha-token>}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
COMPOSE="$ROOT/scripts/mastodon-differential-compose.yml"

command -v docker >/dev/null || { echo "!! no docker on PATH" >&2; exit 1; }

echo "==> Starting Mastodon $(grep -o 'mastodon:v[0-9.]*' "$COMPOSE" | head -1)"
docker compose -f "$COMPOSE" up -d

echo "==> Waiting for it to serve"
# Production mode forces SSL, so it answers 301 without this header — which is
# what a reverse proxy in front of it would send anyway.
until curl -sf -m 3 -o /dev/null -H "X-Forwarded-Proto: https" \
        http://localhost:3000/api/v1/instance; do
  sleep 5
done

echo "==> Minting tokens"
# A freshly seeded user is unapproved, and an unapproved user 403s every
# authenticated endpoint — which looks like agreement if you are only comparing
# status codes, so approve it explicitly.
# Two accounts: the interaction comparison needs someone to follow and block.
CREDS=$(docker compose -f "$COMPOSE" exec -T web bin/rails runner '
  %w(differ other).each do |name|
    account = Account.find_or_create_by!(username: name) { |a| a.domain = nil }
    user = User.find_by(email: "#{name}@localhost") || User.create!(
      email: "#{name}@localhost", password: SecureRandom.hex(16), account: account,
      agreement: true, approved: true, confirmed_at: Time.now.utc)
    # A seeded user is unapproved, and an unapproved user answers 403 to every
    # authenticated endpoint — which looks like agreement if only status codes
    # are compared.
    user.update!(approved: true, confirmed_at: Time.now.utc)
    app = Doorkeeper::Application.find_or_create_by!(name: "app-#{name}") do |a|
      a.redirect_uri = "urn:ietf:wg:oauth:2.0:oob"
      a.scopes = "read write follow push"
    end
    token = Doorkeeper::AccessToken.find_or_create_by!(
      application: app, resource_owner_id: user.id, revoked_at: nil
    ) { |t| t.scopes = "read write follow push"; t.expires_in = nil }
    puts "#{name}:#{token.token}:#{user.account_id}"
  end
' | tr -d '\r')
TOKEN=$(echo "$CREDS" | grep '^differ:' | cut -d: -f2)
OTHER_ID=$(echo "$CREDS" | grep '^other:' | cut -d: -f3)

echo "==> Comparing"
python3 "$ROOT/scripts/differential_test.py" \
  --eunha "$EUNHA_URL" --mastodon http://localhost:3000 \
  --eunha-token "$EUNHA_TOKEN" --mastodon-token "$TOKEN" \
  --eunha-other-id "${EUNHA_OTHER_ID:-}" --mastodon-other-id "$OTHER_ID"
