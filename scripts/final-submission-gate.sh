#!/usr/bin/env bash
set -euo pipefail

# Runs the final read-only regression gate and requires the reviewed tree to
# be clean so the submitted source matches the verified artifacts.
repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

if [[ -n "$(git status --short)" ]]; then
  echo "final submission gate requires a clean worktree" >&2
  git status --short >&2
  exit 1
fi

scripts/submission-readiness-gate.sh

if [[ -n "$(git status --short)" ]]; then
  echo "final submission gate changed tracked state unexpectedly" >&2
  git status --short >&2
  exit 1
fi

printf '%s\n' 'Final local submission regression passed.'
printf '%s\n' 'External evidence remains operator-supplied: hosted URL, repository URL, tester sessions, finalized proof records, screenshots, videos, and retained League report.'
