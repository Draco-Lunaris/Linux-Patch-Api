#!/bin/bash
# Bump version across all version source files for linux_patch_api
# Usage: ./scripts/bump-version.sh <new_version> <old_version>
# Example: ./scripts/bump-version.sh 1.1.18 1.1.17
set -euo pipefail

NEW_VERSION="${1:?Usage: bump-version.sh <new_version> <old_version>}"
OLD_VERSION="${2:?Usage: bump-version.sh <new_version> <old_version>}"

REPO_DIR="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_DIR"

echo "=== Bumping version from $OLD_VERSION to $NEW_VERSION ==="
echo ""

# 1. Cargo.toml (PRIMARY)
sed -i "s/^version = \"$OLD_VERSION\"/version = \"$NEW_VERSION\"/" Cargo.toml
echo "[1/3] Cargo.toml: $OLD_VERSION -> $NEW_VERSION"

# 2. debian/changelog - Prepend new entry using temp file
TEMP_CHANGELOG=$(mktemp)
echo "linux-patch-api ($NEW_VERSION) unstable; urgency=low" > "$TEMP_CHANGELOG"
echo "" >> "$TEMP_CHANGELOG"
echo "  * Release v$NEW_VERSION" >> "$TEMP_CHANGELOG"
echo "" >> "$TEMP_CHANGELOG"
echo " -- Draco-Lunaris-Echo  $(date -R)" >> "$TEMP_CHANGELOG"
echo "" >> "$TEMP_CHANGELOG"
cat debian/changelog >> "$TEMP_CHANGELOG"
mv "$TEMP_CHANGELOG" debian/changelog
echo "[2/3] debian/changelog: Added entry for $NEW_VERSION"

# 3. install.sh - Use generic pattern to match any VERSION value
if [ -f install.sh ]; then
    sed -i "s/^VERSION=\".*\"/VERSION=\"$NEW_VERSION\"/" install.sh
    echo "[3/3] install.sh: -> $NEW_VERSION"
else
    echo "[3/3] install.sh: Not found, skipping"
fi

# 4. linux-patch-api.spec (uses VERSION_PLACEHOLDER, no update needed)
if grep -q 'VERSION_PLACEHOLDER' linux-patch-api.spec 2>/dev/null; then
    echo "[4/4] linux-patch-api.spec: Uses VERSION_PLACEHOLDER (derived at build time)"
else
    echo "[4/4] linux-patch-api.spec: WARNING - does not use VERSION_PLACEHOLDER"
fi

echo ""
echo "=== Version bump complete ==="
echo ""
echo "Verification:"
echo "  Cargo.toml:        $(grep '^version' Cargo.toml)"
echo "  debian/changelog:  $(head -1 debian/changelog)"
if [ -f install.sh ]; then
    echo "  install.sh:        $(grep '^VERSION=' install.sh)"
fi

echo ""
echo "Stale references check:"
grep -r "$OLD_VERSION" --include='*.toml' --include='*.sh' --include='*.json' --include='changelog' --include='control' --include='*.spec' . 2>/dev/null | grep -v 'target/' | grep -v '.git/' | grep -v 'bump-version.sh' || echo "  No stale references found"

echo ""
echo "Next steps:"
echo "  1. Review changes: git diff"
echo "  2. Commit: git commit -am 'chore: bump version to $NEW_VERSION'"
echo "  3. Push: git push origin master"
echo "  4. Tag: git tag v$NEW_VERSION && git push origin v$NEW_VERSION"
echo "  5. Create release via GitHub Releases"
