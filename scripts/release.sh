#!/usr/bin/env sh
# Cut a release: bump both workspaces to one version, refresh their lockfiles,
# commit, and print the two commands that finish it. It never pushes and never
# tags — those are the steps a person takes after CI is green.
#
#   scripts/release.sh 0.1.5
#
# Why a script at all: the version lives in two manifests because `crates/app`
# is its own workspace (GPUI must stay out of the engine build), and each
# workspace has its own lockfile that restates its crates' versions. That is
# four files for one number, and a miss fails `release.yml`'s `verify` *after*
# the tag is pushed. The tag and both manifests must agree, or the workflow
# refuses to ship a mislabeled build.
#
# What it checks before touching anything, because each failure is cheaper here
# than after a tag:
#
#   - the version is semver and ahead of the app's current one;
#   - `CHANGELOG.md` already has a `## <version>` section (the release body is
#     that section, and `ci.yml` fails a version with no entry);
#   - the tree is clean, so the release commit is the bump and nothing else;
#   - both manifests currently agree, which catches a half-finished bump from
#     an earlier attempt.
#
# Then it prints, rather than runs:
#
#   git push origin master          # wait for this commit's ci.yml to finish
#   git tag -s <version> -m …       # only once that run is green
#   git push origin <version>
#
# The order matters: `release.yml` step 3 reads `ci.yml`'s check runs for the
# tag's SHA and fails with "never ran for <sha>" if the tag is pushed beside the
# commit instead of after it. That is what forces a re-tag.
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$root"

usage() {
    sed -n '2,9p' "$0" | sed 's/^# \{0,1\}//' >&2
    exit 2
}

version=${1:-}
[ -n "$version" ] || usage

# Semver, no prefix and no prerelease suffix: `release.yml` refuses a tag whose
# version carries one, because `releases/latest` never serves a prerelease.
if ! printf '%s' "$version" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$'; then
    echo "release: '$version' is not a bare semver version (expected e.g. 0.1.5)" >&2
    exit 1
fi

read_version() {
    sed -n 's/^version *= *"\(.*\)"/\1/p' "$1" | head -n 1
}

app_version=$(read_version crates/app/Cargo.toml)
root_version=$(read_version Cargo.toml)

if [ "$root_version" != "$app_version" ]; then
    echo "release: the workspaces disagree (root $root_version, app $app_version);" >&2
    echo "         finish or revert that bump before cutting a release" >&2
    exit 1
fi

if [ "$version" = "$app_version" ]; then
    echo "release: already at $version; nothing to bump" >&2
    exit 1
fi

if [ "$(printf '%s\n%s\n' "$app_version" "$version" | sort -t. -k1,1n -k2,2n -k3,3n | tail -n 1)" != "$version" ]; then
    echo "release: $version is not ahead of $app_version" >&2
    exit 1
fi

if ! grep -q "^## $version\$" CHANGELOG.md; then
    echo "release: CHANGELOG.md has no '## $version' section." >&2
    echo "         The release body is that section, and ci.yml fails without it." >&2
    exit 1
fi

if [ -n "$(git status --porcelain)" ]; then
    echo "release: the tree is dirty; the release commit must be the bump alone" >&2
    git status --short >&2
    exit 1
fi

# Both manifests, then each workspace's lockfile through cargo so the versions
# inside them are rewritten by the tool that owns their format.
sed -i.bak "s/^version *= *\"$root_version\"/version = \"$version\"/" Cargo.toml
sed -i.bak "s/^version *= *\"$app_version\"/version = \"$version\"/" crates/app/Cargo.toml
rm -f Cargo.toml.bak crates/app/Cargo.toml.bak

for crate in restbook-core restbook-store restbook-transport restbook-devcli; do
    cargo update -p "$crate" --offline >/dev/null 2>&1 || true
done

(
    cd crates/app
    for crate in restbook-app restbook-core restbook-store restbook-transport; do
        cargo update -p "$crate" --offline >/dev/null 2>&1 || true
    done
)

# The lockfiles are the evidence the bump took everywhere it had to.
changed=$(git status --porcelain)
for file in Cargo.toml crates/app/Cargo.toml Cargo.lock crates/app/Cargo.lock; do
    if ! printf '%s' "$changed" | grep -q " $file\$"; then
        echo "release: $file did not change; the bump is incomplete" >&2
        exit 1
    fi
done

git add Cargo.toml crates/app/Cargo.toml Cargo.lock crates/app/Cargo.lock
git commit -m "release: $version" >/dev/null

cat >&2 <<EOF

release: bumped to $version and committed.

Next, in this order:

  1. git push origin master
  2. wait for that commit's ci.yml run to finish green
  3. git tag -s $version -m "restbook $version" && git push origin $version

Do not tag beside the push: release.yml reads the check runs for the tag's
SHA and fails with "never ran for <sha>".

Keep -m on the tag: "git tag -s <version> -m ..." signs with the SSH key
"user.signingkey" names, and the message settles the editor. Without -m git
opens one (vi, with no core.editor), which a scripted release never answers.
EOF
