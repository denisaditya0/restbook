#!/usr/bin/env sh
# Build the desktop app into `dist/` as an artifact that can be installed or run,
# with the per-OS branding (icon, bundle/desktop metadata, taskbar identity)
# applied.
#
#   scripts/package.sh            # debug build, this OS's artifact
#   scripts/package.sh --release  # optimized
#
# Windows, macOS and Linux each take a different shape, because each OS reads
# branding from a different place:
#
#   Windows  a bare exe plus the DLLs it imports. The icon and version block are
#            compiled *into* the binary by `crates/app/build.rs`, so there is
#            nothing to copy beside it except the crypto library. (This needs the
#            MSVC toolchain: see the note in `build.rs` about GNU `ld` dropping
#            the resource section.)
#   macOS    a `.app` bundle. `Info.plist` and `Contents/Resources/<name>.icns`
#            are the branding; macOS reads neither out of the executable.
#   Linux    a tarball laid out as an install prefix: the binary under `bin/`, a
#            `.desktop` entry and the hicolor icon set under `share/`. The
#            window's `app_id` has to match the `.desktop` file name.
#
# The engine's C dependency (SQLCipher -> libcrypto) is dynamic on Windows and
# Linux, so the matching shared library is copied next to the binary. macOS links
# CommonCrypto into the system and needs nothing.
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
target="$root/crates/app/target"
out="$root/dist"

profile=debug
flag=
if [ "${1:-}" = "--release" ]; then
    profile=release
    flag=--release
fi

# One place the version is read for every artifact name. The app crate holds its
# own version (it is a separate workspace); the tag has to match it or the
# release workflow refuses to ship.
version=$(sed -n 's/^version *= *"\(.*\)"/\1/p' "$root/crates/app/Cargo.toml" | head -n 1)

# The app id, bundle name and icon stem, read from the source of truth rather
# than repeated here. `branding.rs` is a Rust file, so this is a scrape, not a
# dependency — but a scrape that fails loudly is better than a copy that drifts.
branding="$root/crates/app/src/branding.rs"
app_id=$(grep -o 'APP_ID: &str = "[^"]*"' "$branding" | sed 's/.*"\(.*\)"/\1/')
name=$(grep -o 'pub(crate) const NAME: &str = "[^"]*"' "$branding" | sed 's/.*"\(.*\)"/\1/')
[ -n "$app_id" ] && [ -n "$name" ] || {
    echo "package: could not read the app id from $branding" >&2
    exit 1
}

echo "package: $name $version, app id $app_id, profile $profile" >&2

case "$(uname -s)" in
    MINGW*|MSYS*|CYGWIN*) os=windows ;;
    Darwin) os=macos ;;
    *) os=linux ;;
esac

# --- Windows: an exe and the crypto DLL, nothing else -----------------------
#
# The icon and version block are in the binary already (build.rs), so this is
# just the copy step. Inno Setup wraps it into `restbook-<v>.exe` for release;
# this bare directory is what that installer is built from, and is runnable on
# its own.
package_windows() {
    ( cd "$root/crates/app" && sh "$root/scripts/dev.sh" build $flag )

    openssl_dir=$(sh "$root/scripts/dev.sh" --print-openssl-dir 2>/dev/null || true)

    mkdir -p "$out"
    cp "$target/$profile/restbook-app.exe" "$out/"

    # `libsqlite3-sys` links libcrypto with the `dylib` kind, so the built exe
    # imports it at load time. Windows' loader searches the exe's directory
    # first, which is why the DLL is copied next to it instead of relying on
    # PATH.
    if [ -n "$openssl_dir" ] && [ -f "$openssl_dir/bin/libcrypto-3-x64.dll" ]; then
        cp "$openssl_dir/bin/libcrypto-3-x64.dll" "$out/"
    fi

    echo "packaged into $out" >&2
    ls -1 "$out" >&2
}

# --- macOS: a .app bundle ---------------------------------------------------
#
# `Info.plist` and the `.icns` are the branding; the Dock and the application
# menu read nothing else. Not signed or notarized here — an unsigned bundle runs
# locally (arm64 gets an ad-hoc linker signature) but needs right-click -> Open
# after download.
package_macos() {
    ( cd "$root/crates/app" && cargo build $flag )

    app="$out/$name.app"
    rm -rf "$app"
    mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"

    cp "$target/$profile/restbook-app" "$app/Contents/MacOS/"

    icon="$root/crates/app/icons/$name.icns"
    if [ -f "$icon" ]; then
        cp "$icon" "$app/Contents/Resources/"
    else
        echo "package: $icon missing; run scripts/icons.sh on macOS first" >&2
    fi

    sed -e "s|@APP_ID@|$app_id|g" \
        -e "s|@NAME@|$name|g" \
        -e "s|@VERSION@|$version|g" \
        -e "s|@ICON@|$name|g" \
        "$root/crates/app/packaging/Info.plist" > "$app/Contents/Info.plist"

    # Only meaningful on macOS, and harmless to skip elsewhere.
    if command -v codesign >/dev/null 2>&1; then
        codesign --force --deep --sign - "$app" 2>/dev/null \
            || echo "package: ad-hoc codesign failed; the bundle still runs locally" >&2
    fi

    echo "packaged into $app" >&2
}

# --- Linux: an install-prefix tarball ---------------------------------------
#
# The window's `app_id` is what ties the running app to the icon and the
# `.desktop` entry, so all three carry `$app_id`: the entry's file name,
# `StartupWMClass`, and the hicolor icon file name.
package_linux() {
    ( cd "$root/crates/app" && cargo build $flag --no-default-features )

    prefix="$out/$name-$version-linux"
    rm -rf "$prefix"
    mkdir -p "$prefix/bin" "$prefix/share/applications"

    cp "$target/$profile/restbook-app" "$prefix/bin/"

    # Every hicolor size the icon set has. A launcher picks the closest one.
    for png in "$root/crates/app/icons/$name"-*.png; do
        [ -f "$png" ] || continue
        size=$(basename "$png" | sed -n 's/.*-\([0-9]*\)\.png/\1/p')
        dir="$prefix/share/icons/hicolor/${size}x${size}/apps"
        mkdir -p "$dir"
        cp "$png" "$dir/$app_id.png"
    done

    sed -e "s|@APP_ID@|$app_id|g" -e "s|@NAME@|$name|g" \
        "$root/crates/app/packaging/restbook.desktop" \
        > "$prefix/share/applications/$app_id.desktop"

    ( cd "$out" && tar czf "$name-$version-linux-$(uname -m).tar.gz" "$(basename "$prefix")" )

    echo "packaged into $prefix (.tar.gz beside it)" >&2
}

# Dispatch after every function is defined: `sh` reads a script top to bottom,
# so a `case` above the definitions would not find them.
case "$os" in
    windows) package_windows ;;
    macos) package_macos ;;
    linux) package_linux ;;
esac
