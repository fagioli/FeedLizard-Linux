#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 3 ]]; then
  echo "usage: $0 REPOSITORY_URL PUBLIC_KEY_FILE OUTPUT" >&2
  exit 2
fi

repository_url="$1"
public_key_file="$2"
output="$3"
gpg_key="$(base64 < "$public_key_file" | tr -d '\n')"

{
  printf '[Flatpak Ref]\n'
  printf 'Name=io.github.feedlizard.FeedLizard\n'
  printf 'Branch=release\n'
  printf 'Title=FeedLizard\n'
  printf 'Url=%s\n' "$repository_url"
  printf 'IsRuntime=false\n'
  printf 'RuntimeRepo=https://flathub.org/repo/flathub.flatpakrepo\n'
  printf 'GPGKey=%s\n' "$gpg_key"
} > "$output"
