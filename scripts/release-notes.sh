#!/bin/sh
# Prints a release's notes: the first-launch notice, then what's new in <tag>.
# What's new is the tag's message for an annotated tag (git tag -a vX.Y.Z -F
# notes.md), otherwise the subjects of the commits since the previous tag,
# leaving out those that only touch tests, CI or docs.
#
#   scripts/release-notes.sh v1.2.3
set -eu
tag=$1
cd "$(dirname "$0")/.."

# actions/checkout can leave an annotated tag as a lightweight one: fetch it again.
git fetch --quiet --force origin "refs/tags/$tag:refs/tags/$tag" 2>/dev/null || true

if [ "$(git cat-file -t "$tag")" = tag ]; then
	notes=$(git tag -l --format='%(contents)' "$tag")
else
	prev=$(git describe --tags --abbrev=0 "$tag^" 2>/dev/null || true)
	range=$tag
	[ -n "$prev" ] && range="$prev..$tag"
	notes=$(git log --no-merges --format='%H' "$range" | while read -r c; do
		if git diff-tree --no-commit-id --name-only -r "$c" | grep -qvE '^(tests|egui-ui/tests|sys/tests|bench|docs|\.github)/'; then
			git log -1 --format='- %s' "$c"
		fi
	done)
fi

cat .github/release-notice.md
if [ -n "$notes" ]; then
	printf '\n### What'\''s new\n\n%s\n' "$notes"
fi
