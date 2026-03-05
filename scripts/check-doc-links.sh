#!/usr/bin/env bash
set -euo pipefail

errors=0

while IFS= read -r file; do
  [[ -f "${file}" ]] || continue
  dir="$(dirname "${file}")"
  while IFS= read -r raw_link; do
    link="${raw_link}"

    # Drop optional title and surrounding angle brackets.
    link="${link%% *}"
    link="${link#<}"
    link="${link%>}"

    # Skip non-file links.
    case "${link}" in
      http://*|https://*|mailto:*|tel:*|\#*|javascript:*)
        continue
        ;;
    esac

    # Remove fragment and query.
    link="${link%%#*}"
    link="${link%%\?*}"
    [[ -z "${link}" ]] && continue

    if [[ "${link}" = /* ]]; then
      target="${link}"
    else
      target="${dir}/${link}"
    fi

    if [[ ! -e "${target}" ]]; then
      echo "[doc-links] missing: ${file} -> ${raw_link}"
      errors=1
    fi
  done < <(perl -nle 'while (/\[[^\]]+\]\(([^)]+)\)/g) { print $1 }' "${file}")
done < <(git ls-files '*.md')

if [[ "${errors}" -ne 0 ]]; then
  echo "[doc-links] failed."
  exit 1
fi

echo "[doc-links] pass."
