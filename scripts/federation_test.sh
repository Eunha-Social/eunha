#!/bin/bash
# Federate eunha with a real Mastodon 4.7.0, both directions, and check what lands.
#
# eunha's other federation tests run eunha against eunha, where both sides share
# eunha's reading of ActivityPub — so a misreading is invisible. This builds a
# pair that shares nothing but the specification.
#
# Needs: a container runtime, mkcert, caddy, and a built `target/release/eunha`.
# Everything else is set up here and torn down at the end.
#
#   scripts/federation_test.sh
#   scripts/federation_test.sh --keep      # leave both running to poke at
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORK="${FEDERATION_WORK_DIR:-/tmp/eunha-federation}"
KEEP=""
[ "${1:-}" = "--keep" ] && KEEP=1

EUNHA_DOMAIN="host.docker.internal:3002"   # what the container can reach
EUNHA_PLAIN_PORT=3003                       # eunha itself, behind the proxy
MASTODON_DOMAIN="localhost:3000"            # what the host can reach

for tool in docker mkcert caddy psql; do
  command -v "$tool" >/dev/null || { echo "!! $tool is not on PATH" >&2; exit 1; }
done
[ -x "$ROOT/target/release/eunha" ] || {
  echo "!! build eunha first: cargo build --release --bin eunha" >&2; exit 1; }

cleanup() {
  [ -n "$KEEP" ] && { echo "==> Left running (--keep): eunha :3002, Mastodon :3000"; return; }
  echo "==> Tearing down"
  pkill -f "caddy run --config $WORK/Caddyfile" 2>/dev/null || true
  pkill -f "$ROOT/target/release/eunha" 2>/dev/null || true
  docker compose -f "$WORK/docker-compose.yml" down -v >/dev/null 2>&1 || true
  dropdb --if-exists eunha_federation 2>/dev/null || true
}
trap cleanup EXIT

mkdir -p "$WORK"
cp "$ROOT/scripts/mastodon-differential-compose.yml" "$WORK/docker-compose.yml"

echo "==> Certificates"
# A local CA in the *login* keychain: `mkcert -install` wants root for the system
# store, and a user-level trust root is enough for both sides here — reqwest
# resolves to native-tls, which asks the Security framework either way.
CAROOT="$(mkcert -CAROOT)"
[ -f "$CAROOT/rootCA.pem" ] || mkcert -install 2>/dev/null || true
if ! security verify-cert -c "$CAROOT/rootCA.pem" -p ssl >/dev/null 2>&1; then
  security add-trusted-cert -r trustRoot \
    -k "$HOME/Library/Keychains/login.keychain-db" "$CAROOT/rootCA.pem" 2>/dev/null || true
fi
mkcert -cert-file "$WORK/fed.pem" -key-file "$WORK/fed-key.pem" \
  host.docker.internal localhost 127.0.0.1 >/dev/null 2>&1
cp "$CAROOT/rootCA.pem" "$WORK/mkcert-rootCA.crt"
cp "$WORK/fed.pem" "$WORK/eunha.pem"
cp "$WORK/fed-key.pem" "$WORK/eunha-key.pem"

cat > "$WORK/Caddyfile.eunha" <<EOF
{
	admin off
	auto_https off
}
:3002 {
	tls $WORK/fed.pem $WORK/fed-key.pem
	reverse_proxy 127.0.0.1:$EUNHA_PLAIN_PORT
}
EOF
cat > "$WORK/Caddyfile" <<'EOF'
{
	admin off
	auto_https off
}
:3000 {
	tls /certs/eunha.pem /certs/eunha-key.pem
	reverse_proxy web:3000 {
		header_up X-Forwarded-Proto https
	}
}
EOF

echo "==> Mastodon"
docker compose -f "$WORK/docker-compose.yml" up -d
until curl -sf -m 3 -o /dev/null https://$MASTODON_DOMAIN/api/v1/instance; do sleep 5; done

MASTODON_TOKEN=$(docker compose -f "$WORK/docker-compose.yml" exec -T web bin/rails runner '
  a = Account.find_or_create_by!(username: "masto") { |x| x.domain = nil }
  u = User.find_by(email: "masto@localhost") || User.create!(
    email: "masto@localhost", password: SecureRandom.hex(16), account: a,
    agreement: true, approved: true, confirmed_at: Time.now.utc)
  u.update!(approved: true, confirmed_at: Time.now.utc)
  app = Doorkeeper::Application.find_or_create_by!(name: "fed") do |x|
    x.redirect_uri = "urn:ietf:wg:oauth:2.0:oob"; x.scopes = "read write follow"
  end
  t = Doorkeeper::AccessToken.find_or_create_by!(
    application: app, resource_owner_id: u.id, revoked_at: nil
  ) { |x| x.scopes = "read write follow"; x.expires_in = nil }
  puts t.token' | tr -d '\r' | tail -1)

echo "==> eunha"
dropdb --if-exists eunha_federation 2>/dev/null || true
createdb eunha_federation
psql -q -d eunha_federation -c "CREATE SCHEMA IF NOT EXISTS eunha" >/dev/null
( cd "$ROOT" && SQLX_OFFLINE=true cargo sqlx migrate run \
    --database-url "postgres://$(whoami)@localhost/eunha_federation?options=-c%20search_path%3Deunha,public" >/dev/null )

psql -q -d eunha_federation >/dev/null <<'SQL'
INSERT INTO accounts (id, username, domain, display_name, note, created_at, updated_at)
VALUES (1, 'alice', NULL, 'Alice', '', now(), now()) ON CONFLICT DO NOTHING;
INSERT INTO users (id, email, account_id, created_at, updated_at, confirmed_at, approved, encrypted_password)
VALUES (1, 'alice@localhost', 1, now(), now(), now(), true, 'x') ON CONFLICT DO NOTHING;
INSERT INTO oauth_applications (id, name, uid, secret, redirect_uri, scopes, created_at, updated_at)
VALUES (1, 'fed', 'u', 's', 'urn:ietf:wg:oauth:2.0:oob', 'read write follow push', now(), now())
ON CONFLICT DO NOTHING;
INSERT INTO oauth_access_tokens (id, token, resource_owner_id, application_id, scopes, created_at)
VALUES (1, 'eunha-federation-token', 1, 1, 'read write follow push', now()) ON CONFLICT DO NOTHING;
SQL

VAPID_KEY=$(openssl ecparam -genkey -name prime256v1 -noout 2>/dev/null)
VAPID_PRIV=$(printf '%s' "$VAPID_KEY" | openssl pkcs8 -topk8 -nocrypt 2>/dev/null)
VAPID_PUB=$(printf '%s' "$VAPID_KEY" | openssl ec -pubout -outform DER 2>/dev/null | tail -c 65 | base64 | tr '+/' '-_' | tr -d '=')
cat > "$WORK/config.toml" <<EOF
database_url = "postgres://$(whoami)@localhost/eunha_federation"
redis_url = "redis://127.0.0.1:6379/13"
bind_address = "127.0.0.1:$EUNHA_PLAIN_PORT"

[instance]
domain = "$EUNHA_DOMAIN"
title = "eunha"
description = ""
short_description = ""
contact_email = "alice@localhost"
registrations_open = false
approval_required = false
vapid_private_key = """
$VAPID_PRIV
"""
vapid_public_key = "$VAPID_PUB"
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
from = "alice@localhost"
EOF

( cd "$WORK" && nohup "$ROOT/target/release/eunha" > "$WORK/eunha.log" 2>&1 & )
nohup caddy run --config "$WORK/Caddyfile.eunha" > "$WORK/caddy.log" 2>&1 &
until curl -sf -m 3 -o /dev/null "https://localhost:3002/api/v1/instance"; do sleep 2; done

echo "==> Federating"
python3 "$ROOT/scripts/federation_test.py" \
  --eunha "https://localhost:3002" \
  --eunha-token "eunha-federation-token" \
  --eunha-acct "alice@$EUNHA_DOMAIN" \
  --mastodon "https://$MASTODON_DOMAIN" \
  --mastodon-token "$MASTODON_TOKEN" \
  --mastodon-acct "masto@$MASTODON_DOMAIN"
