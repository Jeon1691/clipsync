#!/usr/bin/env bash
# Copyright the ClipSync contributors.
# SPDX-License-Identifier: MIT
# Re-read both remote refs immediately before publishing release artifacts.
set -euo pipefail
source_ref="${GITHUB_REF:?missing GITHUB_REF}"
source_sha="${GITHUB_SHA:?missing GITHUB_SHA}"
repository="${GITHUB_REPOSITORY:?missing GITHUB_REPOSITORY}"
[[ "$source_ref" =~ ^refs/tags/v[0-9]+\.[0-9]+\.[0-9]+$ ]]
[[ "$source_sha" =~ ^([0-9a-f]{40}|[0-9a-f]{64})$ ]]
default_branch="$(gh api "repos/${repository}" --jq .default_branch)"
git check-ref-format "refs/heads/${default_branch}"
# Force only local tracking/scratch refs: a corrected remote tag must be read,
# then rejected if it no longer identifies this workflow's source commit.
git fetch --no-tags origin \
  "+refs/heads/${default_branch}:refs/remotes/origin/${default_branch}" \
  "+${source_ref}:refs/clipsync-release-check/tag"
if [[ "$(git rev-parse --verify 'refs/clipsync-release-check/tag^{commit}')" != "$source_sha" ]]; then
  echo 'release tag does not identify this workflow source commit' >&2
  exit 1
fi
if ! git merge-base --is-ancestor "$source_sha" "refs/remotes/origin/${default_branch}"; then
  echo 'release source must be an ancestor of the current default branch' >&2
  exit 1
fi
