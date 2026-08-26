#!/bin/bash
set -e

export PATH="$HOME/.orbstack/bin:$PATH"

# Homebrew keeps postgresql off the login PATH, and this script needs `psql`
# before sqlx can connect. Take whichever versioned formula is installed rather
# than pinning one, so a major-version upgrade does not stop the deploy at the
# `CREATE SCHEMA` below — which is where it stopped, after `git pull` and before
# anything was built.
for pgbin in /opt/homebrew/opt/postgresql@*/bin; do
  [ -d "$pgbin" ] && export PATH="$pgbin:$PATH"
done

# Derive the database name from config.toml so this script is instance-agnostic.
# config.toml's database_url is the container's view (host.docker.internal); the
# host-side migration connects via localhost, so we only reuse the DB name.
DB_NAME=$(sed -nE 's/^database_url[[:space:]]*=[[:space:]]*"?.*\/([^"/]+)"?[[:space:]]*$/\1/p' config.toml | head -1)
if [ -z "$DB_NAME" ]; then
  echo "deploy.sh: could not determine database name from config.toml" >&2
  exit 1
fi
export DATABASE_URL="postgres://limeburst@localhost/${DB_NAME}"

git pull

# Migrations run before the new container starts, and the server refuses to
# serve a database behind its binary, so a failure here stops the deploy with
# the old version still serving. The sqlx CLI does it rather than `eunha
# migrate` because the binary is built inside the image, not on this host.
#
# Keep sqlx's _sqlx_migrations bookkeeping table in the eunha schema, matching
# the app's connection search_path, so the public schema stays a pure Mastodon
# mirror. The schema must exist before `migrate run` creates the table.
psql "$DATABASE_URL" -c "CREATE SCHEMA IF NOT EXISTS eunha"
DATABASE_URL="${DATABASE_URL}?options=-c%20search_path%3Deunha,public" sqlx migrate run

docker compose up -d --build
