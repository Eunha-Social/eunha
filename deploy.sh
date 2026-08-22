#!/bin/bash
set -e

export PATH="$HOME/.orbstack/bin:$PATH"

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

# Keep sqlx's _sqlx_migrations bookkeeping table in the eunha schema, matching
# the app's connection search_path, so the public schema stays a pure Mastodon
# mirror. The schema must exist before `migrate run` creates the table.
psql "$DATABASE_URL" -c "CREATE SCHEMA IF NOT EXISTS eunha"
DATABASE_URL="${DATABASE_URL}?options=-c%20search_path%3Deunha,public" sqlx migrate run

# The server refuses to start against a database behind its binary, so a failure
# here stops the deploy with the old version still serving.

docker compose up -d --build
