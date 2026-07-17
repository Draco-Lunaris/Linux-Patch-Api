#!/bin/sh
# Upload build artifacts to GitHub Release
# Usage: upload-release.sh <tag_name> <file_path>
# Example: upload-release.sh v1.0.0 "../linux-patch-api_1.0.0-1_amd64.deb"
#
# Required environment variables:
#   GITHUB_TOKEN - API token with repo access (or GH_TOKEN)

set -e

TAG_NAME="${1:?Usage: upload-release.sh <tag_name> <file_path>}"
FILE_PATH="${2}"

REPO="Draco-Lunaris/Linux-Patch-Api"

TOKEN="${GITHUB_TOKEN:-${GH_TOKEN:-}}"
if [ -z "$TOKEN" ]; then
    echo "Error: GITHUB_TOKEN (or GH_TOKEN) environment variable not set"
    exit 1
fi

if [ -z "$FILE_PATH" ] || [ ! -f "$FILE_PATH" ]; then
    echo "No file found at '$FILE_PATH'"
    echo "Skipping upload."
    exit 0
fi

echo "Uploading $(basename "$FILE_PATH") for release $TAG_NAME..."

# Use gh CLI if available, otherwise fall back to curl
if command -v gh >/dev/null 2>&1; then
    gh release upload "$TAG_NAME" "$FILE_PATH" --repo "$REPO" --clobber
    echo "Successfully uploaded $(basename "$FILE_PATH") to release $TAG_NAME"
    exit 0
fi

# Fall back to GitHub REST API
API_BASE="https://api.github.com/repos/$REPO"

# Try to find existing release
RELEASE_ID=$(curl -s -H "Authorization: token $TOKEN" \
    "$API_BASE/releases/tags/$TAG_NAME" | grep -o '"id":[0-9]*' | head -1 | cut -d: -f2)

# Create release if it doesn't exist
if [ -z "$RELEASE_ID" ]; then
    echo "Creating new release for tag $TAG_NAME..."
    RESPONSE=$(curl -s -X POST \
        -H "Authorization: token $TOKEN" \
        -H "Content-Type: application/json" \
        -d "{\"tag_name\": \"$TAG_NAME\", \"name\": \"$TAG_NAME\"}" \
        "$API_BASE/releases")
    RELEASE_ID=$(echo "$RESPONSE" | grep -o '"id":[0-9]*' | head -1 | cut -d: -f2)
fi

if [ -z "$RELEASE_ID" ]; then
    echo "Error: Could not create or find release for tag $TAG_NAME"
    exit 1
fi

# Upload the asset
UPLOAD_URL=$(curl -s -H "Authorization: token $TOKEN" \
    "$API_BASE/releases/$RELEASE_ID" | grep -o '"upload_url":"[^"]*"' | head -1 | cut -d'"' -f4 | sed 's/{?name,label}//')

UPLOAD_RESPONSE=$(curl -s -w "\nHTTP_CODE:%{http_code}" -X POST \
    -H "Authorization: token $TOKEN" \
    -H "Content-Type: application/octet-stream" \
    --data-binary "@$FILE_PATH" \
    "${UPLOAD_URL}?name=$(basename "$FILE_PATH")")

HTTP_CODE=$(echo "$UPLOAD_RESPONSE" | grep "HTTP_CODE:" | cut -d: -f2)
if [ "$HTTP_CODE" != "201" ] && [ "$HTTP_CODE" != "200" ]; then
    echo "Upload failed with HTTP code $HTTP_CODE"
    echo "$UPLOAD_RESPONSE"
    exit 1
fi

echo "Successfully uploaded $(basename "$FILE_PATH") to release $TAG_NAME"