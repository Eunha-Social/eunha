#!/usr/bin/env bash
# Rehearse pending migrations against a copy of a live database.
#
# Usage: scripts/rehearse_migration.sh <source-database-url> [clone-name]
#
# Migrations run at startup, so on a real instance they get exactly one attempt
# against data nobody has tested them on. This clones that data, runs the
# pending migrations over the clone the way the server would, and reports what
# changed: per-table row counts before and after, and whether the resulting
# schema still matches the Mastodon release eunha tracks.
#
# The source database is only read from. Requires pg_dump/pg_restore, psql, and
# the sqlx CLI; run it on the database host, where cloning does not cross a
# network.

set -euo pipefail

SOURCE="${1:?Usage: $0 <source-database-url> [clone-name]}"
CLONE_NAME="${2:-rehearsal_$(date +%Y%m%d_%H%M%S)}"
PGBIN="${PGBIN:-}"
DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(dirname "$DIR")"

# Same database server, different database.
CLONE="${SOURCE%/*}/${CLONE_NAME}"
# The server sets this on every connection so that sqlx's ledger stays out of
# the Mastodon schema; the rehearsal has to match, or sqlx will not find the
# migrations already applied and will try to run them all again.
CLONE_WITH_PATH="${CLONE}?options=-c%20search_path%3Deunha,public"

DUMP="$(mktemp -t eunha-rehearsal).dump"
trap 'rm -f "$DUMP" "$DUMP.before" "$DUMP.after"' EXIT

row_counts() {
    "${PGBIN}psql" -tA "$1" -c "
        SELECT c.relname || '=' || (xpath(
                   '/row/cnt/text()',
                   query_to_xml(format('SELECT count(*) AS cnt FROM public.%I', c.relname), false, true, '')
               ))[1]::text
        FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
        WHERE n.nspname = 'public' AND c.relkind = 'r'
        ORDER BY c.relname;"
}

echo "==> Cloning into '$CLONE_NAME' (the source is only read) ..."
"${PGBIN}pg_dump" -Fc -Z1 -d "$SOURCE" -f "$DUMP"
"${PGBIN}dropdb" --if-exists "$CLONE_NAME"
"${PGBIN}createdb" "$CLONE_NAME"
"${PGBIN}pg_restore" -j4 --no-owner --no-privileges -d "$CLONE" "$DUMP"

echo "==> Row counts before ..."
row_counts "$CLONE" > "$DUMP.before"

echo "==> Running pending migrations ..."
time sqlx migrate run --source "$ROOT/migrations" --database-url "$CLONE_WITH_PATH"

echo "==> Row counts after ..."
row_counts "$CLONE" > "$DUMP.after"

echo "==> What changed"
if diff "$DUMP.before" "$DUMP.after"; then
    echo "    (no table gained or lost rows)"
fi

echo "==> Schema check"
if command -v eunha-schema >/dev/null 2>&1; then
    eunha-schema check --database-url "$CLONE"
else
    (cd "$ROOT" && SQLX_OFFLINE=true cargo run --quiet --bin eunha-schema -- \
        check --database-url "$CLONE")
fi

echo
echo "==> Rehearsal database '$CLONE_NAME' left in place; drop it when done:"
echo "    dropdb $CLONE_NAME"
