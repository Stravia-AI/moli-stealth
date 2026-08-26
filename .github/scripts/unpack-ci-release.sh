#!/usr/bin/env bash
set -euo pipefail

if [[ $# -lt 1 || $# -gt 2 ]]; then
  echo "usage: $0 head|base [EXPECTED_REVISION]" >&2
  exit 2
fi

side=$1
case "$side" in
  head|base) ;;
  *) echo "invalid release side: $side" >&2; exit 2 ;;
esac

release_name="moli-release-$side"
archive="target/ci-artifacts/$side/$release_name.tar.gz"
test -f "$archive"
mkdir -p target/ci-bin
tar -xzf "$archive" -C target/ci-bin
(cd "target/ci-bin/$release_name" && sha256sum --check SHA256SUMS)
if [[ $# -eq 2 ]]; then
  actual_revision=$(<"target/ci-bin/$release_name/revision.txt")
  if [[ "$actual_revision" != "$2" ]]; then
    echo "$side artifact revision mismatch: expected $2, got $actual_revision" >&2
    exit 1
  fi
fi
