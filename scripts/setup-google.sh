#!/usr/bin/env bash
# setup-google.sh PROJECT_ID — create a Google Cloud project for easyCAD sync and
# enable the Drive API, then print the console pages for the steps gcloud cannot
# do for a personal account (see crates/sync/README.md).
set -euo pipefail
p=${1:?usage: setup-google.sh PROJECT_ID}
command -v gcloud >/dev/null || { echo "gcloud not found: https://cloud.google.com/sdk/docs/install" >&2; exit 1; }
gcloud projects describe "$p" >/dev/null 2>&1 || gcloud projects create "$p"
gcloud services enable drive.googleapis.com --project "$p"
cat <<MSG
Drive API enabled for $p. Now, in order:
  1. Branding:     https://console.cloud.google.com/auth/branding?project=$p
  2. Audience:     https://console.cloud.google.com/auth/audience?project=$p
                   External; add yourself under Test users (or Publish app)
  3. Data access:  https://console.cloud.google.com/auth/scopes?project=$p
                   add https://www.googleapis.com/auth/drive.file
  4. Clients:      https://console.cloud.google.com/auth/clients?project=$p
                   Create client > Desktop app > Download JSON
  5. cargo run -p easycad-sync --example login -- --client-file <downloaded.json>
MSG
