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
# Needs a container runtime (OrbStack, Docker Desktop, colima). Either point it
# at an eunha you are already running, or give it no arguments and it will bring
# up one of its own against a scratch database:
#
#   scripts/differential_test.sh                              # its own eunha
#   scripts/differential_test.sh http://localhost:3001 TOKEN  # one you run
#
# The first form exists so that CI and a developer run the same path. The eunha
# side needs a migrated database, two accounts and a token, and while that lived
# in nobody's script it lived in nobody's memory either — which is how this
# harness went weeks without being run at all.
#
# Set EUNHA_OTHER_ID to a second account on the eunha side to include the
# interaction verbs — follow, block, mute — which otherwise compare nothing.
# It is set for you when this script brings up its own eunha.
#
# For that form: EUNHA_PORT (3001), EUNHA_DB (eunha_differential),
# EUNHA_DATABASE_URL (a local socket to EUNHA_DB), EUNHA_REDIS_URL, and
# DIFFERENTIAL_WORK_DIR. `createdb`, `dropdb` and `psql` take their connection
# from the usual PG* variables, so a Postgres that wants a host and a password —
# CI's — is reached by setting those alongside EUNHA_DATABASE_URL.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
COMPOSE="$ROOT/scripts/mastodon-differential-compose.yml"
OWN_EUNHA=""
[ $# -eq 0 ] && OWN_EUNHA=1
if [ -z "$OWN_EUNHA" ]; then
  EUNHA_URL="${1:?usage: differential_test.sh [<eunha-url> <eunha-token>]}"
  EUNHA_TOKEN="${2:?usage: differential_test.sh [<eunha-url> <eunha-token>]}"
fi

command -v docker >/dev/null || { echo "!! no docker on PATH" >&2; exit 1; }

# ── An eunha of our own, when none was given ──────────────────────────────────
#
# Torn down at the end; the Mastodon side is left up, because bringing it back
# costs a minute and rerunning against it costs seconds.
EUNHA_PORT="${EUNHA_PORT:-3001}"
EUNHA_DB="${EUNHA_DB:-eunha_differential}"
WORK="${DIFFERENTIAL_WORK_DIR:-/tmp/eunha-differential}"

cleanup() {
  [ -n "$OWN_EUNHA" ] || return 0
  if [ -n "${EUNHA_PID:-}" ]; then
    kill "$EUNHA_PID" 2>/dev/null || true
    for _ in $(seq 20); do
      kill -0 "$EUNHA_PID" 2>/dev/null || break
      sleep 0.2
    done
    kill -9 "$EUNHA_PID" 2>/dev/null || true
    wait "$EUNHA_PID" 2>/dev/null || true
  fi
  # The database is left behind on purpose — it is the evidence when a run
  # fails, and the next run drops it before doing anything else.
  return 0
}
trap cleanup EXIT

start_own_eunha() {
  command -v psql >/dev/null || { echo "!! no psql on PATH" >&2; exit 1; }
  [ -x "$ROOT/target/release/eunha" ] || {
    echo "!! build eunha first: cargo build --release --bin eunha" >&2; exit 1; }

  rm -rf "$WORK"; mkdir -p "$WORK"

  # `PGDATABASE` and friends carry the connection; a URL would have to be
  # reassembled for `createdb`, `psql` and eunha separately and they would drift.
  echo "==> Preparing $EUNHA_DB"
  dropdb --if-exists "$EUNHA_DB"
  createdb "$EUNHA_DB"
  local url
  url="${EUNHA_DATABASE_URL:-postgres:///$EUNHA_DB}"

  # Migration 001 creates the `eunha` schema, and the search path eunha connects
  # with puts sqlx's own ledger in it — so `public` stays a pure mirror of
  # Mastodon's schema, which is what the schema check depends on.
  #
  # From `$WORK`, because a checkout usually has a `config.toml` and a `.env`
  # naming somebody's real development database, and this is the one command
  # here that would write to whichever one it found.
  ( cd "$WORK" && DATABASE_URL="$url" "$ROOT/target/release/eunha" migrate )

  # Two accounts, because the interaction verbs need someone to follow and
  # block, and a token to act as the first of them. No signing keys: nothing
  # here federates.
  psql -q -d "$EUNHA_DB" -v ON_ERROR_STOP=1 >/dev/null <<'SQL'
INSERT INTO accounts (id, username, domain, display_name, note, created_at, updated_at)
VALUES (1, 'differ', NULL, 'Differ', '', now(), now()),
       (2, 'other',  NULL, 'Other',  '', now(), now());
INSERT INTO users (id, email, account_id, created_at, updated_at, confirmed_at, approved, encrypted_password)
VALUES (1, 'differ@localhost', 1, now(), now(), now(), true, 'x'),
       (2, 'other@localhost',  2, now(), now(), now(), true, 'x');
INSERT INTO oauth_applications (id, name, uid, secret, redirect_uri, scopes, created_at, updated_at)
VALUES (1, 'differential', 'u', 's', 'urn:ietf:wg:oauth:2.0:oob', 'read write follow push', now(), now());
INSERT INTO oauth_access_tokens (id, token, resource_owner_id, application_id, scopes, created_at)
VALUES (1, 'eunha-differential-token', 1, 1, 'read write follow push', now());
SQL

  local vapid vapid_priv vapid_pub
  vapid=$(openssl ecparam -genkey -name prime256v1 -noout 2>/dev/null)
  vapid_priv=$(printf '%s' "$vapid" | openssl pkcs8 -topk8 -nocrypt 2>/dev/null)
  vapid_pub=$(printf '%s' "$vapid" | openssl ec -pubout -outform DER 2>/dev/null     | tail -c 65 | base64 | tr '+/' '-_' | tr -d '=')

  cat > "$WORK/config.toml" <<EOF
database_url = "$url"
redis_url = "${EUNHA_REDIS_URL:-redis://127.0.0.1:6379/14}"
bind_address = "127.0.0.1:$EUNHA_PORT"

[instance]
domain = "localhost:$EUNHA_PORT"
title = "eunha"
description = ""
short_description = ""
contact_email = "differ@localhost"
registrations_open = false
approval_required = false
vapid_private_key = """
$vapid_priv
"""
vapid_public_key = "$vapid_pub"
privacy_policy = ""
terms_of_service = ""

[media_storage]
bucket = "f"
region = "auto"
endpoint = "http://localhost:9999"
access_key_id = "f"
secret_access_key = "f"
base_url = "http://localhost:9999"

[resend]
api_key = ""
from = "differ@localhost"
EOF

  echo "==> Starting eunha on :$EUNHA_PORT"
  # `exec`, so that `$!` is eunha's own pid. Backgrounding the `cd && eunha`
  # list instead gives the pid of the shell wrapping it, and cleanup then kills
  # something that has already gone while eunha keeps the port.
  ( cd "$WORK"; exec "$ROOT/target/release/eunha" > "$WORK/eunha.log" 2>&1 ) &
  EUNHA_PID=$!
  for _ in $(seq 60); do
    curl -sf -m 3 -o /dev/null "http://127.0.0.1:$EUNHA_PORT/api/v1/instance" && break
    sleep 1
  done
  curl -sf -m 3 -o /dev/null "http://127.0.0.1:$EUNHA_PORT/api/v1/instance" || {
    echo "!! eunha did not come up:" >&2; tail -30 "$WORK/eunha.log" >&2; exit 1; }

  EUNHA_URL="http://127.0.0.1:$EUNHA_PORT"
  EUNHA_TOKEN="eunha-differential-token"
  EUNHA_OTHER_ID=2
}

[ -n "$OWN_EUNHA" ] && start_own_eunha

echo "==> Starting Mastodon $(grep -o 'mastodon:v[0-9.]*' "$COMPOSE" | head -1)"
docker compose -f "$COMPOSE" up -d

echo "==> Waiting for it to serve"
# Production mode forces SSL, so it answers 301 without this header — which is
# what a reverse proxy in front of it would send anyway.
until curl -sf -m 3 -o /dev/null -H "X-Forwarded-Proto: https" \
        http://localhost:3000/api/v1/instance; do
  sleep 5
done

# A worker, or nothing that Mastodon defers ever happens — and some of that is
# visible in the API. `unfavourite` and `unreblog` hand the removal to
# `UnfavouriteWorker` and `RemovalWorker` and force the flag false in their own
# response, so the undo itself looks right while the row survives; every later
# request then reads it and reports `favourited: true` on a status that was
# unfavourited. That is what nine of these findings were, blamed on eunha for
# weeks. Sidekiq registers itself in Redis, so ask Redis rather than trusting
# that a container was started.
echo "==> Waiting for a worker"
# Anything other than a number — Redis not up yet, the exec failing — counts as
# no worker. Left as an empty string it would go to `[ "" -gt 0 ]`, which is an
# error rather than a false, and the gate would wave through exactly the case it
# exists to catch.
workers() {
  local n
  n=$(docker compose -f "$COMPOSE" exec -T redis redis-cli scard processes 2>/dev/null | tr -d '\r')
  case "$n" in
    "" | *[!0-9]*) echo 0 ;;
    *) echo "$n" ;;
  esac
}
for _ in $(seq 60); do
  [ "$(workers)" -gt 0 ] && break
  sleep 2
done
if [ "$(workers)" -lt 1 ]; then
  echo "!! no Sidekiq worker registered; the comparison would blame eunha for" >&2
  echo "!! Mastodon's deferred work never running. Logs:" >&2
  docker compose -f "$COMPOSE" logs --tail=20 sidekiq >&2
  exit 1
fi

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
