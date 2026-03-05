#!/usr/bin/env bash
set -euo pipefail

index_file="docs/INDEX.md"

if [[ ! -f "${index_file}" ]]; then
  echo "[doc-index] missing ${index_file}"
  exit 1
fi

errors=0

while IFS= read -r doc; do
  base="$(basename "${doc}")"
  if [[ "${base}" == "INDEX.md" ]]; then
    continue
  fi

  if ! grep -Eq "\(${base//./\\.}\)" "${index_file}"; then
    echo "[doc-index] not referenced in INDEX.md: ${base}"
    errors=1
  fi
done < <(find docs -maxdepth 1 -type f -name '*.md' | sort)

if [[ "${errors}" -ne 0 ]]; then
  echo "[doc-index] failed."
  exit 1
fi

echo "[doc-index] pass."
