#!/usr/bin/env bash
# One FFmpeg-Builds stage's source, fetched as the stage fetches it for a build: its own
# ffbuild_dockerdl (git at a commit, svn at a revision, submodules, a checksum-checked tarball,
# crates vendored by cargo), run in FFmpeg-Builds' base image as its download.sh runs it. The
# result is archived as one tar.xz. The download commands go beside it (<out>.dl) for MANIFEST.txt.
# A Rust stage whose download does not vendor its crates (rav1e: its build fetches them from
# crates.io) gets them vendored here, at the versions of its Cargo.lock.
#
#   dl_stage.sh <stage script> <out .tar.xz>      (in the base image, with /dl_functions.sh)
set -xe -o pipefail
shopt -s dotglob
source /dl_functions.sh
source "$1"
STG="$(ffbuild_dockerdl)"
if [[ -z "$STG" ]]; then
  exit 0
fi
printf '%s\n' "$STG" > "$2.dl"
WORK="$(mktemp -d)"
trap 'rm -rf -- "$WORK"' EXIT
cd "$WORK"
eval "set -e; $STG"
if [[ -f Cargo.lock && ! -d vendor ]]; then
  CARGO_HOME="$(mktemp -d)" cargo vendor --locked --versioned-dirs vendor > /dev/null
  printf '%s\n' "cargo vendor --locked --versioned-dirs vendor" >> "$2.dl"
fi
tar -I "xz -T0" -cpf "$2" .
