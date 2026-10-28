#!/usr/bin/env bash
# Downloads the pinned English Wikipedia parquet shards wf-bench reads
# (issue #440) into bench/data/, verifying each by SHA-256. Re-running skips
# shards that are already present and valid.
#
# Source: huggingface.co/datasets/wikimedia/wikipedia, config 20231101.en,
# pinned to one dataset revision so the corpus never shifts under a run.
#
# Usage: bench/fetch-wiki.sh [shard-count]   (default 4, ~1.7 GB, ~640k articles)
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DATA="$HERE/data"
REV=b04c8d1ceb2f5cd4588862100d08de323dccfbaa
BASE="https://huggingface.co/datasets/wikimedia/wikipedia/resolve/$REV/20231101.en"
SHA256=(
  382e7f6f09e488b24793a7f7cfc659879d5a22da2cf2efec6491665f0c019677
  6cd57a5825312ca966313d4a1b614690b5c4fb53cee9b124bc0e999977eb36cf
  b18674f163d4f106e43396fd58412a79b80a418c72bb728c20af439e7ca4d5b7
  0b8a6f452f5ef59d0986a067f70110feb16d7c1d281ba2db0b3df32933618fcc
)
COUNT="${1:-4}"
if (( COUNT < 1 || COUNT > ${#SHA256[@]} )); then
  echo "shard-count must be 1-${#SHA256[@]}" >&2
  exit 1
fi

mkdir -p "$DATA"
for ((i = 0; i < COUNT; i++)); do
  name="$(printf 'wiki-%05d.parquet' "$i")"
  path="$DATA/$name"
  want="${SHA256[$i]}"
  if [[ -f "$path" ]] && echo "$want  $path" | sha256sum -c --status; then
    echo "ok $name"
    continue
  fi
  curl -fL --retry 3 -o "$path.part" "$BASE/$(printf 'train-%05d-of-00041.parquet' "$i")"
  echo "$want  $path.part" | sha256sum -c --status || {
    echo "sha256 mismatch for $name" >&2
    exit 1
  }
  mv "$path.part" "$path"
  echo "ok $name"
done
