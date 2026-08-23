#!/bin/bash
# Record what Mastodon's API entities carry, into mastodon/entities.json.
#
# Reads a local Mastodon checkout at the tracked tag — a hundred-odd files that
# GitHub throttles if fetched one request at a time, and that a clone already
# has. Set MASTODON_REPO to point elsewhere.
#
# Two sources, neither sufficient alone: the REST serializers decide what is
# emitted, and the TypeScript API types state which fields are optional. See
# scripts/extract_entity_types.py.
#
# Usage: scripts/build_entity_types.sh [vX.Y.Z]
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TAG="${1:-v$(grep -E '^version' "$ROOT/mastodon.toml" | head -1 | cut -d'"' -f2)}"
REPO="${MASTODON_REPO:-$HOME/Git/mastodon}"

if [ ! -d "$REPO/.git" ]; then
  echo "!! No Mastodon checkout at $REPO." >&2
  echo "   git clone https://github.com/mastodon/mastodon.git $REPO" >&2
  echo "   or set MASTODON_REPO to an existing one." >&2
  exit 1
fi

if ! git -C "$REPO" rev-parse -q --verify "refs/tags/$TAG" >/dev/null; then
  echo "==> $TAG is not in $REPO; fetching tags"
  # Tags only: never touches the checkout's branch or working tree.
  git -C "$REPO" fetch --tags --quiet origin
  git -C "$REPO" rev-parse -q --verify "refs/tags/$TAG" >/dev/null || {
    echo "!! $REPO has no tag $TAG even after fetching." >&2
    exit 1
  }
fi

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
mkdir -p "$WORK/rb" "$WORK/ts"

# Read the tag rather than the working tree, so an unrelated checkout state or
# local edit cannot end up recorded as upstream's.
extract() {
  local tree="$1" dest="$2" count=0
  while IFS= read -r path; do
    git -C "$REPO" show "$TAG:$path" > "$WORK/$dest/$(echo "${path#"$tree/"}" | tr '/' '_')"
    count=$((count + 1))
  done < <(git -C "$REPO" ls-tree -r --name-only "$TAG" "$tree")
  echo "    $count from $tree"
}

echo "==> Reading Mastodon $TAG from $REPO"
extract app/serializers/rest rb
extract app/javascript/mastodon/api_types ts

echo "==> Extracting entity shapes"
python3 "$ROOT/scripts/extract_entity_types.py" "$WORK/rb" "$WORK/ts" "$ROOT/mastodon/entities.json"
echo "Wrote $ROOT/mastodon/entities.json"
