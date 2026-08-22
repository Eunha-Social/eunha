#!/usr/bin/env bash
# Migrate a Mastodon pg_dump (custom format) into a fresh eunha database.
# Usage: scripts/migrate_from_mastodon.sh dump.custom [old-domain] [new-domain]
#
# Prerequisites:
#   - sqlx CLI on PATH (for running eunha schema migrations)
#   - pg_restore and psql on PATH (or set PGBIN=/path/to/pg/bin/)
#   - DATABASE_URL set (or defaults to postgres:///eunha)
#
# The dump must come from the Mastodon release this eunha tracks (see
# mastodon.toml); the schema eunha builds is that release's schema, and a dump
# from another one does not fit it. Set ALLOW_SCHEMA_MISMATCH=1 to override.

set -euo pipefail

DUMP="${1:?Usage: $0 dump.custom old-domain new-domain}"
OLD="${2:?old-domain required (e.g. seoul.earth)}"
NEW="${3:?new-domain required (e.g. eunha.social)}"
DB="${DATABASE_URL:-postgres:///eunha}"
PGBIN="${PGBIN:-}"
DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(dirname "$DIR")"

# Extract database name from the connection string for dropdb/createdb
DBNAME="${DB##*/}"

# The Mastodon migration eunha's own migrations bring the schema up to.
EXPECTED="$(sed -n 's/^schema_version *= *"\([0-9]*\)".*/\1/p' "$ROOT/mastodon.toml")"
if [ -z "$EXPECTED" ]; then
    echo "error: could not read schema_version from $ROOT/mastodon.toml" >&2
    exit 1
fi

echo "==> Checking the dump's Mastodon schema version ..."
# Mastodon records every migration it has run in public.schema_migrations. The
# newest one identifies the schema the dump's data was written for.
FOUND="$("${PGBIN}pg_restore" --data-only --table=schema_migrations -f - "$DUMP" 2>/dev/null \
    | grep -Eo '^[0-9]{14}$' \
    | sort \
    | tail -1 || true)"

if [ -z "$FOUND" ]; then
    echo "    warning: the dump records no Mastodon migrations, so its schema" >&2
    echo "    version cannot be checked. Continuing; expected $EXPECTED." >&2
elif [ "$FOUND" != "$EXPECTED" ]; then
    echo "    dump is at Mastodon schema $FOUND, but this eunha builds $EXPECTED." >&2
    if [ "${ALLOW_SCHEMA_MISMATCH:-0}" != "1" ]; then
        cat >&2 <<EOF

    Restoring this dump would put data shaped for one schema into another:
    columns added since $FOUND would keep their defaults instead of being
    backfilled, and columns dropped since would have nowhere to go.

    Either upgrade the source Mastodon to the release eunha tracks and dump
    again, or use an eunha release that tracks the source's Mastodon version.
    To proceed anyway: ALLOW_SCHEMA_MISMATCH=1 $0 $*
EOF
        exit 1
    fi
    echo "    ALLOW_SCHEMA_MISMATCH=1 set; continuing anyway." >&2
else
    echo "    dump is at Mastodon schema $FOUND, as expected."
fi

echo "==> Recreating database '$DBNAME' ..."
"${PGBIN}dropdb" --if-exists "$DBNAME"
"${PGBIN}createdb" "$DBNAME"

echo "==> Running eunha schema migrations..."
sqlx migrate run --database-url "$DB"

echo "==> Restoring Mastodon data into $DB ..."
# schema_migrations and ar_internal_metadata are seeded by eunha's own
# migrations (see 007_mastodon_schema_versions.sql), so the dump's copies are
# skipped rather than fought with.
TOC="$(mktemp)"
"${PGBIN}pg_restore" -l "$DUMP" \
    | grep -v "TABLE DATA public ar_internal_metadata\|TABLE DATA public schema_migrations\|TABLE DATA public pghero_space_stats\|SEQUENCE SET public pghero_space_stats_id_seq" \
    > "$TOC"
"${PGBIN}pg_restore" \
    --data-only \
    --no-owner \
    --no-privileges \
    --single-transaction \
    --disable-triggers \
    --use-list="$TOC" \
    -d "$DB" "$DUMP"
rm -f "$TOC"

echo "==> Applying fixups (${OLD} -> ${NEW}) ..."
"${PGBIN}psql" "$DB" \
    -v "old_domain=${OLD}" \
    -v "new_domain=${NEW}" \
    -f "$DIR/migrate_from_mastodon.sql"

echo "==> Verifying the result against Mastodon's schema ..."
if command -v eunha-schema >/dev/null 2>&1; then
    eunha-schema check --database-url "$DB"
else
    echo "    eunha-schema not on PATH; run \`mise run schema:check\` to verify."
fi

echo "==> Done."
