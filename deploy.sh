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
git submodule sync
git submodule update --init
sqlx migrate run
docker compose up -d --build
