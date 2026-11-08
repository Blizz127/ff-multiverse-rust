#!/usr/bin/env bash
# scripts/package.sh - build and package a PRIVATE FF Multiverse release tarball.
#
# Usage: scripts/package.sh [--skip-build [--bin-dir DIR]] [-h|--help]
#
# Output (under <repo>/dist/, which is git-ignored):
#   ff-multiverse-<version>-<gitshort>[-dirty][-prebuilt]-linux-x86_64/     staging dir (kept)
#   ff-multiverse-<version>-<gitshort>[-dirty][-prebuilt]-linux-x86_64.tar.gz
#   ff-multiverse-<version>-<gitshort>[-dirty][-prebuilt]-linux-x86_64.manifest.json (sidecar)
#   SHA256SUMS                                                              "<sha256>  <tarball name>"
#
# Steps:
#   1. cargo build --release --offline --locked for fmv-server, fmv-client, fmv-import.
#      The target directory comes from `cargo metadata`, so CARGO_TARGET_DIR is honoured
#      (a relative CARGO_TARGET_DIR is taken relative to the directory you run this from).
#      With --skip-build (or FMV_SKIP_BUILD=1) nothing is compiled: the binaries are taken
#      as they are from --bin-dir DIR (default <target dir>/release). That mode exists for
#      testing the guard cheaply with dummy files, and it is marked honestly: the package
#      name gets a -prebuilt suffix and manifest.json gets "build_mode": "prebuilt",
#      because the commit, rustc and target fields then describe the packaging host and
#      checkout, not necessarily how the binaries were made. Real releases use the default.
#   2. Stage ONLY the allowlist (ALLOWLIST below): the three binaries, the launcher entry
#      point ff-multiverse.sh (scripts/ff-multiverse.sh, which must pass `bash -n`),
#      README.md, a generated LICENSE-NOTICE.txt and a generated manifest.json.
#   3. Guard: fail if anything staged is not on the allowlist, is not a plain file, or
#      looks like owned game data (*.lgp *.bin *.iso *.cue *.chd *.ff7 *.mcr
#      PROVENANCE.json, matched case-insensitively). The SAME check runs three times:
#      on a pre-existing staging directory (before it is touched), on the freshly staged
#      directory, and on the listing of the finished archive.
#   4. Create a deterministic .tar.gz, then write the sidecar manifest and SHA256SUMS.
#
# Idempotent: re-running replaces this package's staging directory, archive and sidecar.
# A staging directory is only ever removed after it has passed the guard, and then
# file-by-file followed by rmdir, so nothing unrecognised is deleted. If someone left an
# unexpected file there (for example an owned-data file), the run stops and names it.
#
# Environment:
#   CARGO_TARGET_DIR    optional, where cargo builds (see above).
#   SOURCE_DATE_EPOCH   optional, seconds since the epoch used for built_at and archive
#                       mtimes. With it set and identical binaries the archive is
#                       byte-for-byte reproducible.
#   FMV_SKIP_BUILD=1    same as --skip-build.
#
# manifest.json inside the archive cannot contain the archive's own sha256 (that would
# be circular); the sidecar <name>.manifest.json is the same document plus a "tarball"
# object with the archive's file name, size and sha256.
#
# Launcher hand-off (docs/LAUNCHER.md): the tarball name always matches
# ^ff-multiverse-.*linux-(x86_64|x64).*\.tar\.gz$, dist/SHA256SUMS is the checksum asset
# (one line, for the package built by THIS run: it always describes the latest package, so
# publish a package together with the SHA256SUMS its own run wrote), and manifest.json carries
# "revision" (git rev-list --count HEAD) and "build_id" (ff-multiverse-r<revision>-<gitshort>
# plus -dirty/-prebuilt): the launcher offers an update only when the installed and the
# available version labels both contain an r<N> tag and the available N is larger, so publish
# with build_id as the version label. The revision only grows along one branch: publish
# packages built from the integration branch, never from a side branch.
#
# Requires: bash >= 4, cargo, rustc, git, jq, tar (GNU), gzip, sha256sum, wc, date, sort.
# This script never contacts any host and never reads or copies game data.

set -euo pipefail

PRODUCT="ff-multiverse"
PLATFORM="linux-x86_64"
# Cargo packages to release; each builds a binary of the same name.
PACKAGES=(fmv-server fmv-client fmv-import)
# The ONLY names allowed in a package (flat: no subdirectories, no symlinks).
ALLOWLIST=(fmv-server fmv-client fmv-import ff-multiverse.sh README.md LICENSE-NOTICE.txt manifest.json)
# Owned-data lookalikes, lower case. Matched against the lower-cased basename and
# reported with a clearer message than "not in the allowlist". There are no exceptions:
# no *.bin file may ever ship.
DENY_PATTERNS=('*.lgp' '*.bin' '*.bin.orig' '*.iso' '*.cue' '*.chd' '*.ff7' '*.mcr' 'provenance.json')

TMP_FILES=()

log() { printf 'package.sh: %s\n' "$*" >&2; }
err() { printf 'package.sh: error: %s\n' "$*" >&2; }
die() { err "$*"; exit 1; }

usage() {
  cat <<'EOF'
Usage: scripts/package.sh [--skip-build [--bin-dir DIR]] [-h|--help]

Builds release binaries for fmv-server, fmv-client and fmv-import, stages an explicit
allowlist (the three binaries, the launcher entry point ff-multiverse.sh, README.md, a
LICENSE-NOTICE.txt and a manifest.json) into
dist/ff-multiverse-<version>-<gitshort>-linux-x86_64/, verifies that no owned game data is
present, and writes a .tar.gz, a sidecar manifest and dist/SHA256SUMS under dist/.
Prints the tarball path, sha256 and launcher build id on success. See the header of this
script for details.

  --skip-build     do not compile; package binaries that already exist. The package is
                   marked -prebuilt (name) and "build_mode": "prebuilt" (manifest).
  --bin-dir DIR    with --skip-build: directory holding fmv-server, fmv-client and
                   fmv-import (default: <cargo target dir>/release).

Environment: CARGO_TARGET_DIR (optional), SOURCE_DATE_EPOCH (optional, reproducible
timestamps), FMV_SKIP_BUILD=1 (same as --skip-build).
EOF
}

cleanup() {
  if ((${#TMP_FILES[@]})); then
    rm -f -- "${TMP_FILES[@]}"
  fi
}

# ---------------------------------------------------------------------------
# The guard. verify_stage, verify_listing and check_package_path print reasons to
# stderr and return non-zero; they never exit, so they can be sourced and tested.
# ---------------------------------------------------------------------------

# is_owned_data_name <path>: true when the basename looks like owned game data.
is_owned_data_name() {
  local lower base pat
  lower="${1,,}"
  base="${lower##*/}"
  for pat in "${DENY_PATTERNS[@]}"; do
    # shellcheck disable=SC2053  # $pat is a glob on purpose
    if [[ $base == $pat ]]; then
      return 0
    fi
  done
  return 1
}

# check_package_path <path relative to the package root>: print the reason and return 1
# unless the path is allowlisted and does not look like owned data.
check_package_path() {
  local rel="$1" allowed
  if is_owned_data_name "$rel"; then
    printf 'matches an owned-data pattern: %q\n' "$rel"
    return 1
  fi
  for allowed in "${ALLOWLIST[@]}"; do
    if [[ $rel == "$allowed" ]]; then
      return 0
    fi
  done
  printf 'not in the allowlist: %q\n' "$rel"
  return 1
}

# verify_stage <dir> <require_complete: 1 = every allowlisted file must exist, 0 = subset ok>
# Flat scan including dotfiles: every entry must be a regular, non-symlink, allowlisted file.
verify_stage() {
  local dir="$1" require_complete="$2" bad=0 entry rel reason allowed
  local -A present=()
  for entry in "$dir"/* "$dir"/.[!.]* "$dir"/..?*; do
    if [[ ! -e $entry && ! -L $entry ]]; then
      continue # a glob pattern that matched nothing
    fi
    rel="${entry##*/}"
    present["$rel"]=1
    if [[ -L $entry ]]; then
      err "  - symlink (not allowed): $rel"
      bad=1
    elif [[ -d $entry ]]; then
      err "  - directory (not allowed): $rel"
      bad=1
    elif [[ ! -f $entry ]]; then
      err "  - special file (not allowed): $rel"
      bad=1
    fi
    if ! reason=$(check_package_path "$rel"); then
      err "  - $reason"
      bad=1
    fi
  done
  if [[ $require_complete == 1 ]]; then
    for allowed in "${ALLOWLIST[@]}"; do
      if [[ -z ${present[$allowed]:-} ]]; then
        err "  - missing: $allowed"
        bad=1
      fi
    done
  fi
  [[ $bad == 0 ]]
}

# verify_listing <tarball> <package name>: the archive must hold exactly the package root
# directory plus the allowlisted files, nothing else.
verify_listing() {
  local tarball="$1" name="$2" listing line rel reason bad=0 want got
  if ! listing=$(tar --list --gzip --file "$tarball"); then
    err "  - cannot list archive: $tarball"
    return 1
  fi
  while IFS= read -r line; do
    if [[ -z $line || $line == "$name/" ]]; then
      continue # the root directory entry carries a trailing slash
    fi
    if [[ $line != "$name/"* ]]; then
      err "  - entry outside the package root: $line"
      bad=1
      continue
    fi
    rel="${line#"$name"/}"
    if ! reason=$(check_package_path "$rel"); then
      err "  - $reason"
      bad=1
    fi
  done <<<"$listing"
  want=$(printf '%s\n' "$name/" "${ALLOWLIST[@]/#/$name/}" | LC_ALL=C sort)
  got=$(LC_ALL=C sort <<<"$listing")
  if [[ $got != "$want" ]]; then
    err "  - archive listing is not exactly the package root plus the ${#ALLOWLIST[@]} allowlisted files"
    bad=1
  fi
  [[ $bad == 0 ]]
}

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

require_tools() {
  local missing=() tool
  if ((BASH_VERSINFO[0] < 4)); then
    die "bash >= 4 is required (found $BASH_VERSION)"
  fi
  for tool in cargo rustc git jq tar gzip sha256sum wc date sort cp chmod rm rmdir mkdir mv; do
    if ! command -v "$tool" >/dev/null 2>&1; then
      missing+=("$tool")
    fi
  done
  if ((${#missing[@]})); then
    die "missing required tools: ${missing[*]}"
  fi
}

# sha256_of <file>: hex digest only. Reading stdin avoids any file-name parsing.
sha256_of() {
  local out
  out=$(sha256sum <"$1") || die "sha256sum failed for $1"
  out="${out%% *}"
  [[ $out =~ ^[0-9a-f]{64}$ ]] || die "unexpected sha256sum output for $1: $out"
  printf '%s' "$out"
}

# size_of <file>: size in bytes.
size_of() {
  local out
  out=$(wc -c <"$1") || die "wc failed for $1"
  printf '%s' "${out//[[:space:]]/}"
}

# check_launcher_script <file>: the launcher entry point must be a plain (non-symlink) bash
# script that parses, or it must not ship.
check_launcher_script() {
  local file="$1" first
  if [[ -L $file || ! -f $file ]]; then
    err "the launcher entry point is not a plain file: $file"
    return 1
  fi
  IFS= read -r first <"$file" || true
  if [[ $first != '#!/usr/bin/env bash' ]]; then
    err "the launcher entry point must start with '#!/usr/bin/env bash': $file"
    return 1
  fi
  if ! "$BASH" -n "$file"; then
    err "the launcher entry point does not parse (bash -n): $file"
    return 1
  fi
}

# prepare_stage <dir>: make <dir> an empty directory. An existing directory is deleted
# only if it passes the guard; deletion is file-by-file then rmdir, never rm -rf.
prepare_stage() {
  local dir="$1" allowed
  if [[ -L $dir || (-e $dir && ! -d $dir) ]]; then
    die "$dir exists and is not a plain directory; remove it by hand"
  fi
  if [[ -d $dir ]]; then
    log "found an existing staging directory; auditing it before reuse"
    if ! verify_stage "$dir" 0; then
      die "refusing to touch the existing staging directory $dir (entries listed above are not plain allowlisted files). Inspect it, remove the offending entries, then re-run."
    fi
    for allowed in "${ALLOWLIST[@]}"; do
      rm -f -- "$dir/$allowed"
    done
    rmdir -- "$dir" || die "could not remove staging directory $dir; something unexpected is still inside"
  fi
  mkdir -- "$dir"
  chmod 0755 -- "$dir"
}

# ---------------------------------------------------------------------------
# main
# ---------------------------------------------------------------------------

main() {
  local skip_build=0 bin_dir=""
  if [[ ${FMV_SKIP_BUILD:-0} == 1 ]]; then
    skip_build=1
  fi
  while (($#)); do
    case "$1" in
      -h | --help)
        usage
        return 0
        ;;
      --skip-build)
        skip_build=1
        ;;
      --bin-dir)
        if (($# < 2)); then
          err "--bin-dir needs a directory"
          return 2
        fi
        bin_dir="$2"
        shift
        ;;
      *)
        err "unknown argument: $1"
        usage >&2
        return 2
        ;;
    esac
    shift
  done
  if [[ -n $bin_dir && $skip_build == 0 ]]; then
    err "--bin-dir only applies together with --skip-build"
    return 2
  fi

  require_tools

  # A relative CARGO_TARGET_DIR (or --bin-dir) means "relative to where the user ran this".
  if [[ -n ${CARGO_TARGET_DIR:-} && $CARGO_TARGET_DIR != /* ]]; then
    export CARGO_TARGET_DIR="$PWD/$CARGO_TARGET_DIR"
  fi
  if [[ -n $bin_dir ]]; then
    bin_dir=$(cd -- "$bin_dir" 2>/dev/null && pwd -P) || die "--bin-dir is not a directory"
  fi
  if [[ -n ${CARGO_BUILD_TARGET:-} ]]; then
    die "CARGO_BUILD_TARGET is set; cross builds are not supported (the package name says $PLATFORM)"
  fi

  local script_dir repo_root dist_dir
  script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
  repo_root=$(cd -- "$script_dir/.." && pwd -P)
  dist_dir="$repo_root/dist"
  cd -- "$repo_root"
  [[ -f Cargo.toml && -f README.md ]] || die "run from a checkout that has Cargo.toml and README.md at its root ($repo_root)"

  # Turn the signals that can arrive (Ctrl-C, kill, closed terminal, closed pipe) into
  # ordinary exits: temp files are always removed and the status is the conventional
  # 128+signal.
  trap cleanup EXIT
  trap 'exit 130' INT
  trap 'exit 143' TERM
  trap 'exit 129' HUP
  trap 'exit 141' PIPE

  # --- metadata: version, target dir, license ------------------------------
  local meta pkg version target_dir license
  meta=$(cargo metadata --format-version 1 --no-deps --offline --locked) ||
    die "cargo metadata failed (is Cargo.lock up to date? --locked is required for releases)"
  for pkg in "${PACKAGES[@]}"; do
    jq -e --arg p "$pkg" 'any(.packages[]; .name == $p)' <<<"$meta" >/dev/null ||
      die "workspace package not found: $pkg"
  done
  version=$(jq -r '.packages[] | select(.name == "fmv-server") | .version' <<<"$meta")
  target_dir=$(jq -r '.target_directory' <<<"$meta")
  license=$(jq -r '[.packages[] | select(.name == "fmv-server") | .license][0] // "UNKNOWN"' <<<"$meta")
  [[ $version =~ ^[0-9A-Za-z][0-9A-Za-z.+_-]*$ ]] || die "unusable fmv-server version: '$version'"
  [[ -n $target_dir ]] || die "cargo metadata returned no target directory"

  # --- toolchain -------------------------------------------------------------
  local rustc_version rustc_vv target_triple="" line
  rustc_version=$(rustc --version) || die "rustc --version failed"
  rustc_vv=$(rustc -vV) || die "rustc -vV failed"
  while IFS= read -r line; do
    if [[ $line == "host: "* ]]; then
      target_triple="${line#host: }"
    fi
  done <<<"$rustc_vv"
  [[ $target_triple == x86_64-*-linux-* ]] ||
    die "this script only produces $PLATFORM packages; the rustc host is '${target_triple:-unknown}'"

  # --- build (or, with --skip-build, locate prebuilt binaries) -----------------
  local bin_src_dir="$target_dir/release" build_mode=built
  if ((skip_build)); then
    build_mode=prebuilt
    if [[ -n $bin_dir ]]; then
      bin_src_dir="$bin_dir"
    fi
    log "WARNING: --skip-build: nothing is compiled; packaging PREBUILT binaries from $bin_src_dir (package is marked -prebuilt)"
  else
    local -a pkg_args=()
    for pkg in "${PACKAGES[@]}"; do
      pkg_args+=(-p "$pkg")
    done
    log "cargo build --release --offline --locked ${pkg_args[*]}"
    cargo build --release --offline --locked "${pkg_args[@]}" >&2 || die "cargo build failed"
  fi
  for pkg in "${PACKAGES[@]}"; do
    [[ -f $bin_src_dir/$pkg && -x $bin_src_dir/$pkg ]] ||
      die "expected executable not found: $bin_src_dir/$pkg"
  done

  # --- identity --------------------------------------------------------------
  local git_commit git_short git_status dirty=false git_field git_desc epoch built_at name revision build_id
  git rev-parse --is-inside-work-tree >/dev/null 2>&1 || die "not inside a git work tree; refusing to package an untraceable build"
  git_commit=$(git rev-parse HEAD) || die "cannot resolve HEAD"
  git_short=$(git rev-parse --short=9 HEAD) || die "cannot resolve HEAD"
  revision=$(git rev-list --count HEAD) || die "cannot count the commits of HEAD"
  [[ $revision =~ ^[1-9][0-9]*$ ]] || die "unusable revision '$revision' (git rev-list --count HEAD)"
  git_status=$(git status --porcelain --untracked-files=normal) || die "git status failed"
  git_field="$git_commit"
  git_desc="$git_short"
  if [[ -n $git_status ]]; then
    dirty=true
    git_field="$git_commit-dirty"
    git_desc="$git_short-dirty"
    log "WARNING: the working tree is dirty; this package is marked -dirty and is not a clean release"
  fi
  if [[ $build_mode == prebuilt ]]; then
    git_desc="$git_desc-prebuilt"
  fi

  epoch="${SOURCE_DATE_EPOCH:-}"
  if [[ -z $epoch ]]; then
    epoch=$(date -u +%s)
  fi
  [[ $epoch =~ ^[0-9]+$ ]] || die "SOURCE_DATE_EPOCH must be a number of seconds, got '$epoch'"
  built_at=$(date -u -d "@$epoch" +%Y-%m-%dT%H:%M:%SZ) || die "cannot format timestamp for epoch $epoch"

  name="$PRODUCT-$version-$git_desc-$PLATFORM"
  build_id="$PRODUCT-r$revision-$git_desc"
  local stage_dir="$dist_dir/$name"
  local tarball="$dist_dir/$name.tar.gz"
  local sidecar="$dist_dir/$name.manifest.json"
  log "package: $name (launcher build id: $build_id)"
  check_launcher_script "$repo_root/scripts/ff-multiverse.sh" || die "scripts/ff-multiverse.sh cannot be packaged"

  # --- stage -----------------------------------------------------------------
  mkdir -p -- "$dist_dir"
  prepare_stage "$stage_dir"

  for pkg in "${PACKAGES[@]}"; do
    cp -- "$bin_src_dir/$pkg" "$stage_dir/$pkg"
    chmod 0755 -- "$stage_dir/$pkg"
  done
  cp -- "$repo_root/scripts/ff-multiverse.sh" "$stage_dir/ff-multiverse.sh"
  chmod 0755 -- "$stage_dir/ff-multiverse.sh"
  cp -- "$repo_root/README.md" "$stage_dir/README.md"
  chmod 0644 -- "$stage_dir/README.md"

  cat >"$stage_dir/LICENSE-NOTICE.txt" <<EOF
FF Multiverse - LICENSE NOTICE
==============================

Package : $name
Version : $version
Commit  : $git_field

PROPRIETARY AND PRIVATE. All rights reserved.
This software is not open source. It is provided only to the people the project
owner has given it to directly. Do not copy, publish, resell or host it for anyone
else.

NO GAME DATA IS INCLUDED. This package contains the FF Multiverse programs only.
Final Fantasy, and its characters, names, artwork, music and game data, belong to
their respective owners. This project is not affiliated with, endorsed by or
sponsored by them. To use anything beyond the synthetic FIXTURE test area you must
supply your own legally owned copy of the game and import it locally with
fmv-import. The importer reads your originals read-only. The server and client never
transmit game data to each other; each side reads its own local copy.

The FIXTURE area is synthetic test content. It is not support for any real game.
EOF
  chmod 0644 -- "$stage_dir/LICENSE-NOTICE.txt"

  # --- manifest (in archive) -------------------------------------------------
  local binaries_json='{}' files_json='{}' allow_json file sha size
  for file in "${PACKAGES[@]}"; do
    sha=$(sha256_of "$stage_dir/$file")
    size=$(size_of "$stage_dir/$file")
    binaries_json=$(jq -c --arg k "$file" --arg sha "$sha" --argjson bytes "$size" \
      '. + {($k): {sha256: $sha, bytes: $bytes}}' <<<"$binaries_json")
  done
  for file in ff-multiverse.sh README.md LICENSE-NOTICE.txt; do
    sha=$(sha256_of "$stage_dir/$file")
    size=$(size_of "$stage_dir/$file")
    files_json=$(jq -c --arg k "$file" --arg sha "$sha" --argjson bytes "$size" \
      '. + {($k): {sha256: $sha, bytes: $bytes}}' <<<"$files_json")
  done
  allow_json=$(printf '%s\n' "${ALLOWLIST[@]}" | jq -R . | jq -sc .)

  jq -n \
    --arg name "$name" \
    --arg product "$PRODUCT" \
    --arg version "$version" \
    --argjson revision "$revision" \
    --arg build_id "$build_id" \
    --arg git_commit "$git_field" \
    --argjson git_dirty "$dirty" \
    --arg rustc "$rustc_version" \
    --arg target "$target_triple" \
    --arg built_at "$built_at" \
    --arg build_mode "$build_mode" \
    --arg license "$license" \
    --argjson allowlist "$allow_json" \
    --argjson binaries "$binaries_json" \
    --argjson files "$files_json" \
    '{name: $name, product: $product, version: $version,
      revision: $revision, build_id: $build_id,
      git_commit: $git_commit, git_dirty: $git_dirty,
      rustc: $rustc, target: $target, profile: "release", built_at: $built_at,
      build_mode: $build_mode,
      license: $license, allowlist: $allowlist,
      binaries: $binaries, files: $files}' >"$stage_dir/manifest.json"
  chmod 0644 -- "$stage_dir/manifest.json"

  # --- guard 2: the freshly staged tree ---------------------------------------
  log "verifying the staged tree against the allowlist"
  if ! verify_stage "$stage_dir" 1; then
    die "staged tree failed the allowlist guard (see above); nothing was packaged"
  fi

  # --- archive ----------------------------------------------------------------
  local tmp_tar="$dist_dir/.$name.tar.gz.tmp.$$" tmp_side="$dist_dir/.$name.manifest.json.tmp.$$"
  local sums="$dist_dir/SHA256SUMS" tmp_sums="$dist_dir/.SHA256SUMS.tmp.$$"
  TMP_FILES+=("$tmp_tar" "$tmp_side" "$tmp_sums")
  local -a sorted=() members=()
  local member
  mapfile -t sorted < <(printf '%s\n' "${ALLOWLIST[@]}" | LC_ALL=C sort)
  members=("$name")
  for member in "${sorted[@]}"; do
    members+=("$name/$member")
  done
  # Explicit member list with --no-recursion: tar can only ever add what is named here.
  if ! tar --create --no-recursion --format=gnu \
    --owner=0 --group=0 --numeric-owner --mtime="@$epoch" \
    --directory "$dist_dir" --file - -- "${members[@]}" |
    gzip -n -9 >"$tmp_tar"; then
    die "failed to create the archive"
  fi

  # --- guard 3: the archive that is about to ship ----------------------------
  log "verifying the archive listing against the allowlist"
  if ! verify_listing "$tmp_tar" "$name"; then
    die "archive failed the allowlist guard (see above); not publishing it"
  fi

  # --- sidecar manifest + publish ----------------------------------------------
  local tar_sha tar_size
  tar_sha=$(sha256_of "$tmp_tar")
  tar_size=$(size_of "$tmp_tar")
  jq --arg file "$name.tar.gz" --arg sha "$tar_sha" --argjson bytes "$tar_size" \
    '. + {tarball: {file: $file, bytes: $bytes, sha256: $sha}}' "$stage_dir/manifest.json" >"$tmp_side"

  # The checksum asset a launcher verifies the archive with: "<sha256>  <asset file name>".
  printf '%s  %s\n' "$tar_sha" "$name.tar.gz" >"$tmp_sums"

  # Drop a stale sidecar and checksum file first so a crash between the moves can never
  # leave a mismatched archive/sidecar/SHA256SUMS set behind.
  rm -f -- "$sidecar" "$sums"
  mv -f -- "$tmp_tar" "$tarball"
  mv -f -- "$tmp_side" "$sidecar"
  mv -f -- "$tmp_sums" "$sums"
  (cd -- "$dist_dir" && sha256sum --check --status -- SHA256SUMS) ||
    die "dist/SHA256SUMS does not match $name.tar.gz"

  printf 'tarball:  %s\n' "$tarball"
  printf 'sha256:   %s\n' "$tar_sha"
  printf 'manifest: %s\n' "$sidecar"
  printf 'sums:     %s\n' "$sums"
  printf 'build_id: %s\n' "$build_id"
}

# Keep this as the last statement: bash parses every function above before running
# main, so `--help` exercises a whole-file parse. Sourcing the file (for tests) runs
# nothing.
if [[ ${BASH_SOURCE[0]} == "$0" ]]; then
  main "$@"
fi
