#!/usr/bin/env bash
set -euo pipefail

workspace_version="$({ sed -n '/^\[workspace.package\]/,/^\[/p' Cargo.toml || true; } | sed -n 's/^version = "\([^"]*\)"/\1/p')"
if [[ -z "$workspace_version" ]]; then
  echo "workspace package version is missing" >&2
  exit 1
fi

if [[ "$workspace_version" != 0.9.0-beta.* && "$workspace_version" != 0.*-beta.* && "$workspace_version" != 1.* ]]; then
  echo "unsupported release version: $workspace_version" >&2
  exit 1
fi

if [[ ! -f data/io.github.feedlizard.FeedLizard.metainfo.xml ]] ||
   ! grep -Fq "version=\"$workspace_version\"" data/io.github.feedlizard.FeedLizard.metainfo.xml; then
  echo "AppStream metadata does not contain $workspace_version" >&2
  exit 1
fi

if [[ -n "${GITHUB_REF_NAME:-}" && "$GITHUB_REF_NAME" != "v$workspace_version" ]]; then
  echo "tag $GITHUB_REF_NAME does not match v$workspace_version" >&2
  exit 1
fi

printf '%s\n' "$workspace_version"
