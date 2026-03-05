#!/usr/bin/env bash
set -euo pipefail

base_ref="${1:-}"
head_ref="${2:-HEAD}"

if [[ -z "${base_ref}" ]]; then
  if git rev-parse --verify HEAD~1 >/dev/null 2>&1; then
    base_ref="HEAD~1"
  else
    echo "[rules-gate] single-commit repository, skip."
    exit 0
  fi
fi

if ! git rev-parse --verify "${base_ref}" >/dev/null 2>&1; then
  echo "[rules-gate] base ref not found: ${base_ref}, skip."
  exit 0
fi

changed_files="$(git diff --name-only "${base_ref}" "${head_ref}")"

if [[ -z "${changed_files}" ]]; then
  echo "[rules-gate] no changed files."
  exit 0
fi

changed_rules="$(echo "${changed_files}" | rg '^rules/.+\.(yaml|yml)$' || true)"

if [[ -z "${changed_rules}" ]]; then
  echo "[rules-gate] no rules/*.yaml changes."
  exit 0
fi

changed_mock="$(echo "${changed_files}" | rg '^examples/mock/' || true)"

if [[ -z "${changed_mock}" ]]; then
  echo "[rules-gate] failed."
  echo "[rules-gate] rules changed but no companion mock changes found."
  echo "[rules-gate] changed rules:"
  echo "${changed_rules}" | sed 's/^/  - /'
  echo "[rules-gate] expected at least one companion file under examples/mock/."
  exit 1
fi

echo "[rules-gate] pass."
echo "[rules-gate] changed rules:"
echo "${changed_rules}" | sed 's/^/  - /'
echo "[rules-gate] companion mock changes:"
echo "${changed_mock}" | sed 's/^/  - /'
