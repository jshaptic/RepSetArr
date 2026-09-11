#!/usr/bin/env bash
# Read or bump the crate version in Cargo.toml, keeping Cargo.lock in sync.
#
#   scripts/bump-version.sh current        prints the version, changes nothing
#   scripts/bump-version.sh patch|minor|major
#                                          rewrites Cargo.toml + Cargo.lock, prints the new version
#
# Pure awk so CI needs no toolchain to cut a release; the Docker build is what
# compiles. Only the version line matters, so a targeted rewrite beats
# `cargo update -w` here: no registry access, no chance of moving a dependency.
set -euo pipefail

cd "$(dirname "$0")/.."

bump="${1:-current}"

# The version under [package] — dependency versions are inline tables, so they
# never start a line with `version =`, but the section guard makes that explicit.
read_field() {
  awk -v field="$1" '
    /^[[:space:]]*\[/ { section = $0 }
    section ~ /^\[package\]/ && $0 ~ "^"field"[[:space:]]*=" {
      match($0, /"[^"]*"/)
      print substr($0, RSTART + 1, RLENGTH - 2)
      exit
    }
  ' Cargo.toml
}

name="$(read_field name)"
current="$(read_field version)"

if [ -z "$name" ] || [ -z "$current" ]; then
  echo "Could not read name/version from Cargo.toml" >&2
  exit 1
fi

if [ "$bump" = "current" ]; then
  echo "$current"
  exit 0
fi

# Drop any -pre / +build metadata before doing arithmetic.
core="${current%%-*}"
core="${core%%+*}"
IFS='.' read -r major minor patch <<<"$core"

if ! [[ "$major$minor$patch" =~ ^[0-9]+$ ]]; then
  echo "Version '${current}' is not a plain X.Y.Z we can bump" >&2
  exit 1
fi

case "$bump" in
  major) version="$((major + 1)).0.0" ;;
  minor) version="${major}.$((minor + 1)).0" ;;
  patch) version="${major}.${minor}.$((patch + 1))" ;;
  *)
    echo "Unknown bump '${bump}' (expected current, patch, minor or major)" >&2
    exit 1
    ;;
esac

awk -v new="$version" '
  /^[[:space:]]*\[/ { section = $0 }
  section ~ /^\[package\]/ && !done && /^version[[:space:]]*=/ {
    sub(/"[^"]*"/, "\"" new "\"")
    done = 1
  }
  { print }
' Cargo.toml > Cargo.toml.tmp && mv Cargo.toml.tmp Cargo.toml

# Cargo.lock carries the crate itself as a package entry; the Docker build runs
# with --locked, so a stale entry there fails the build.
if [ -f Cargo.lock ]; then
  awk -v pkg="$name" -v new="$version" '
    $0 == "[[package]]" { in_pkg = 0 }
    /^name[[:space:]]*=/ { in_pkg = ($0 == "name = \"" pkg "\"") }
    in_pkg && !done && /^version[[:space:]]*=/ {
      $0 = "version = \"" new "\""
      done = 1
    }
    { print }
  ' Cargo.lock > Cargo.lock.tmp && mv Cargo.lock.tmp Cargo.lock
fi

echo "$version"
