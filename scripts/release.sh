#!/bin/bash
# Release helper: bump version, commit, tag, push.
# Usage: ./scripts/release.sh <patch|minor|major>
# CI builds the official per-distro packages from the tag.
set -euo pipefail
KIND="${1:?Usage: release.sh <patch|minor|major>}"
cd "$(git rev-parse --show-toplevel)"

OLD=$(grep '^version' Cargo.toml | head -1 | cut -d'"' -f2)
IFS='.' read -r MAJOR MINOR PATCH <<< "$OLD"
case "$KIND" in
    major) NEW="$((MAJOR+1)).0.0" ;;
    minor) NEW="$MAJOR.$((MINOR+1)).0" ;;
    patch) NEW="$MAJOR.$MINOR.$((PATCH+1))" ;;
    *) echo "Invalid kind: $KIND (use patch|minor|major)"; exit 1 ;;
esac

echo "Releasing v$NEW (from $OLD)"
./scripts/bump-version.sh "$NEW" "$OLD"
git add -A
git commit -m "release v$NEW"
git tag "v$NEW"
echo "Pushing tag v$NEW — CI will build the official packages."
git push && git push origin "v$NEW"
