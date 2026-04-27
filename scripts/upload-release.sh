#!/bin/sh
# Upload build artifacts to Gitea Release
# Usage: upload-release.sh <tag_name> <file_path>
# Example: upload-release.sh v1.0.0 "../linux-patch-api_1.0.0-1_amd64.deb"
#
# Required environment variables:
#   GITEA_TOKEN - API token with repo access
#   GITEA_API   - Gitea API base URL (default: https://gitea.moon-dragon.us/api/v1)

set -e

TAG_NAME="${1:?Usage: upload-release.sh <tag_name> <file_path>}"
FILE_PATH="${2}"

GITEA_API="${GITEA_API:-https://gitea-lxc.moon-dragon.us/api/v1}"
REPO="echo/linux_patch_api"

if [ -z "$GITEA_TOKEN" ]; then
    echo "Error: GITEA_TOKEN environment variable not set"
    exit 1
fi

if [ -z "$FILE_PATH" ] || [ ! -f "$FILE_PATH" ]; then
    echo "No file found at '$FILE_PATH'"
    echo "Skipping upload."
    exit 0
fi

echo "Uploading $(basename "$FILE_PATH") for release $TAG_NAME..."

# Try to find existing release (do not use -f flag since 404 is expected for new releases)
RELEASE_ID=$(curl -s -H "Authorization: token $GITEA_TOKEN" "$GITEA_API/repos/$REPO/releases/tags/$TAG_NAME" | grep -o '"id":[0-9]*' | head -1 | cut -d: -f2)

# Create release if it doesn't exist
if [ -z "$RELEASE_ID" ]; then
    echo "Creating new release for tag $TAG_NAME..."
    RESPONSE=$(curl -s -X POST \
        -H "Authorization: token $GITEA_TOKEN" \
        -H "Content-Type: application/json" \
        -d "{\"tag_name\": \"$TAG_NAME\", \"name\": \"$TAG_NAME\"}" \
        "$GITEA_API/repos/$REPO/releases")
    RELEASE_ID=$(echo "$RESPONSE" | grep -o '"id":[0-9]*' | head -1 | cut -d: -f2)
fi

if [ -z "$RELEASE_ID" ]; then
    echo "Error: Could not create or find release for tag $TAG_NAME"
    exit 1
fi

# Upload the asset
UPLOAD_RESPONSE=$(curl -s -w "\nHTTP_CODE:%{http_code}" -X POST \
    -H "Authorization: token $GITEA_TOKEN" \
    -F "attachment=@$FILE_PATH" \
    "$GITEA_API/repos/$REPO/releases/$RELEASE_ID/assets?name=$(basename "$FILE_PATH")")

HTTP_CODE=$(echo "$UPLOAD_RESPONSE" | grep "HTTP_CODE:" | cut -d: -f2)
if [ "$HTTP_CODE" != "201" ] && [ "$HTTP_CODE" != "200" ]; then
    echo "Upload failed with HTTP code $HTTP_CODE"
    echo "$UPLOAD_RESPONSE"
    exit 1
fi

echo "Successfully uploaded $(basename "$FILE_PATH") to release $TAG_NAME"
