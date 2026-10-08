#!/usr/bin/env bash
# STD-003 rule 1, mechanically: the version every user-facing document states must equal the
# version the build declares. Run by CI and by the Pages workflow; run it locally before tagging.
set -euo pipefail
cd "$(dirname "$0")/.."

fail=0
say() { printf '%s\n' "$*" >&2; }

declared=$(sed -n 's/^version = "\([0-9][0-9.]*\)".*/\1/p' Cargo.toml | head -1)
[ -n "$declared" ] || { say "cannot read workspace version from Cargo.toml"; exit 2; }

# Newest dated release section in the changelog ("## [0.0.2] - 2026-10-06"); [Unreleased] is skipped.
changelog=$(sed -n 's/^## \[\([0-9][0-9.]*\)\] - .*/\1/p' CHANGELOG.md | head -1)
[ "$changelog" = "$declared" ] || { say "CHANGELOG.md newest release is $changelog, Cargo.toml says $declared"; fail=1; }

# The newest release section is the GitHub release notes: it must say what is not safe or finished.
section=$(awk -v v="$declared" '$0 ~ "^## \\[" v "\\]" {on=1; next} on && /^## \[/ {exit} on {print}' CHANGELOG.md)
for heading in "### Known issues" "### Upgrade notes"; do
  printf '%s\n' "$section" | grep -q "^$heading" || { say "CHANGELOG.md [$declared] needs a '$heading' section"; fail=1; }
done

grep -q "^\*\*Current release: v$declared\*\*" README.md \
  || { say "README.md must contain the line '**Current release: v$declared**'"; fail=1; }

for page in site/index.html site/install.html; do
  grep -q "v$declared" "$page" || { say "$page does not mention v$declared"; fail=1; }
done

# Every versioned asset link on the install page must point at the declared version.
stale=$(grep -o 'releases/download/v[0-9][0-9.]*' site/install.html | grep -v "v$declared\$" || true)
[ -z "$stale" ] || { say "site/install.html links a different version: $stale"; fail=1; }

# The site must not load anything from other servers (it is a privacy tool's site).
if grep -nE '(src|href)="https?://' site/*.html | grep -vE 'href="https://github\.com/spilloid/rsRewind' ; then
  say "site pages must not reference other hosts (see above)"; fail=1
fi
if grep -nE '<script|@import|url\(https?:' site/*.html site/style.css; then
  say "site must not include scripts or remote CSS (see above)"; fail=1
fi

# Every image the pages reference must exist.
for ref in $(grep -oh 'src="img/[^"]*"' site/*.html | sed 's/src="//;s/"$//' | sort -u); do
  [ -f "site/$ref" ] || { say "site references missing file site/$ref"; fail=1; }
done

if [ "$fail" -eq 0 ]; then echo "docs check ok: v$declared"; fi
exit "$fail"
