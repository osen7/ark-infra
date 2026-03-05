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
changed_status="$(git diff --name-status "${base_ref}" "${head_ref}")"

if [[ -z "${changed_files}" ]]; then
  echo "[rules-gate] no changed files."
  exit 0
fi

added_flat_rules="$(
  echo "${changed_status}" \
    | awk '$1 == "A" { print $2 }' \
    | rg '^rules/[^/]+\.(yaml|yml)$' \
    | rg -v '^rules/manifest\.yaml$' \
    || true
)"

if [[ -n "${added_flat_rules}" ]]; then
  echo "[rules-gate] failed."
  echo "[rules-gate] new flat rules are not allowed. add new rules under rules/<pack>/."
  echo "${added_flat_rules}" | sed 's/^/  - /'
  exit 1
fi

changed_rules="$(
  echo "${changed_files}" \
    | rg '^rules/.+\.(yaml|yml)$' \
    | rg -v '^rules/fixtures/' \
    || true
)"

if [[ -z "${changed_rules}" ]]; then
  echo "[rules-gate] no rules YAML changes (excluding fixtures)."
  exit 0
fi

if [[ "${NO_FIXTURE_NEEDED:-false}" == "true" ]]; then
  echo "[rules-gate] bypassed by no-fixture-needed label."
  exit 0
fi

changed_companion="$(
  echo "${changed_files}" \
    | rg '^(rules/fixtures/|examples/mock/)' \
    || true
)"

if [[ -z "${changed_companion}" ]]; then
  echo "[rules-gate] failed."
  echo "[rules-gate] rules changed but no companion fixtures/mock changes found."
  echo "[rules-gate] changed rules:"
  echo "${changed_rules}" | sed 's/^/  - /'
  echo "[rules-gate] expected at least one companion file under rules/fixtures/ or examples/mock/."
  echo "[rules-gate] or add PR label: no-fixture-needed"
  exit 1
fi

catalog_rules_changed="$(
  echo "${changed_rules}" \
    | rg '^rules/(hardware|interconnect|network|runtime|scheduler|storage|cluster)/.+\.(yaml|yml)$' \
    || true
)"

if [[ -n "${catalog_rules_changed}" ]]; then
  changed_catalog_fixtures="$(
    echo "${changed_files}" \
      | rg '^rules/fixtures/catalog_' \
      || true
  )"
  if [[ -z "${changed_catalog_fixtures}" ]]; then
    echo "[rules-gate] failed."
    echo "[rules-gate] catalog rules changed but no catalog fixture changes found."
    echo "[rules-gate] changed catalog rules:"
    echo "${catalog_rules_changed}" | sed 's/^/  - /'
    echo "[rules-gate] expected at least one companion file under rules/fixtures/catalog_*/"
    echo "[rules-gate] or add PR label: no-fixture-needed"
    exit 1
  fi
fi

echo "[rules-gate] pass."
echo "[rules-gate] changed rules:"
echo "${changed_rules}" | sed 's/^/  - /'
echo "[rules-gate] companion changes:"
echo "${changed_companion}" | sed 's/^/  - /'
