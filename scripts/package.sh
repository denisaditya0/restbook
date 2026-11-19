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
#   Windows  a bare exe plus the DLLs it imports, and — when Inno Setup's
#            `ISCC.exe` is installed — one installer that carries both. The icon
#            and version block are compiled *into* the binary by
#            `crates/app/build.rs`, so there is nothing to copy beside it except
#            the crypto library. (This needs the MSVC toolchain: see the note in
#            `build.rs` about GNU `ld` dropping the resource section.)
#   macOS    a `.app` bundle, and — only where `hdiutil` exists, i.e. macOS —
#            a `.dmg` built around that bundle with an `Applications` symlink
#            beside it, so the image installs by dragging instead of copying a
#            folder. `Info.plist` and `Contents/Resources/<name>.icns` are the
#            branding; macOS reads neither out of the executable.
#   Linux    a tarball laid out as an install prefix: the binary under `bin/`, a
#            `.desktop` entry and the hicolor icon set under `share/`. The
#            window's `app_id` has to match the `.desktop` file name. Plus a
#            `.deb` built over the same tree where `dpkg-deb` exists, because
#            the tarball is what a user unpacks and the `.deb` is what a distro
#            expects to install. dpkg's `Architecture:` field spells the arch
#            its own way (`amd64`), while the file name keeps the release
#            matrix's spelling (`x86_64`) so every artifact for one build
#            shares a suffix.
#
# Artifact names carry `$version` and the release's arch spelling, never
# `uname`'s: see `release_arch`.
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

# The arch suffix every artifact is named with. `uname` says `arm64` for an
# Apple-silicon Mac and `aarch64` for the same CPU on Linux; a release matrix has
# to spell one build one way, so arm64 is folded into aarch64 and everything else
# (`x86_64`) passes through. Windows and macOS artifacts at the same version then
# share a suffix, and Linux's `.deb` adds its own control-field spelling on top.
release_arch() {
    case "$(uname -m)" in
        arm64) echo aarch64 ;;
        *) uname -m ;;
    esac
}

relarch=$(release_arch)

# --- Windows: an exe, the crypto DLL, and (best effort) an installer --------
#
# The icon and version block are in the binary already (build.rs), so the copy
# step is all the bare artifact needs. `dist/` is runnable on its own — unpack
# it anywhere, the DLL is beside the exe — but it is not installable: no
# Start-menu entry, no uninstaller, and a generic taskbar identity. Inno Setup's
# `packaging/restbook.iss` turns that directory into one `.exe` that has all
# three.
package_windows() {
    ( cd "$root/crates/app" && sh "$root/scripts/dev.sh" build $flag )

    openssl_dir=$(sh "$root/scripts/dev.sh" --print-openssl-dir 2>/dev/null || true)

    mkdir -p "$out"
    cp "$target/$profile/restbook-app.exe" "$out/"

    # `libsqlite3-sys` links libcrypto with the `dylib` kind, so the built exe
    # imports it at load time. Windows' loader searches the exe's directory
    # first, which is why the DLL is copied next to it instead of relying on
    # PATH.
    crypto_dll=
    if [ -n "$openssl_dir" ] && [ -f "$openssl_dir/bin/libcrypto-3-x64.dll" ]; then
        cp "$openssl_dir/bin/libcrypto-3-x64.dll" "$out/"
        crypto_dll=libcrypto-3-x64.dll
    fi

    build_installer "$crypto_dll"

    echo "packaged into $out" >&2
    ls -1 "$out" >&2
}

# Inno Setup's command-line compiler. `choco install innosetup` does not put it
# on PATH, and MSYS does not know the `Program Files` spelling, so the known
# install locations are tried directly.
find_iscc() {
    for iscc in \
        /c/Program\ Files\ \(x86\)/Inno\ Setup\ 6/ISCC.exe \
        /c/Program\ Files/Inno\ Setup\ 6/ISCC.exe \
        /c/Program\ Files\ \(x86\)/Inno\ Setup\ 7/ISCC.exe \
        /c/Program\ Files/Inno\ Setup\ 7/ISCC.exe; do
        if [ -f "$iscc" ]; then
            echo "$iscc"
            return 0
        fi
    done
    command -v iscc 2>/dev/null && return 0
    return 1
}

# ISCC.exe is a Windows binary, so every path it is handed has to be a Windows
# path even though this script is POSIX sh under MSYS.
winpath() {
    if command -v cygpath >/dev/null 2>&1; then
        cygpath -w "$1"
    else
        printf '%s\n' "$1"
    fi
}

# Compile `packaging/restbook.iss` into `$out/restbook-<v>-windows-<arch>.exe`.
# Skipped, with the reason, when the pieces an installer needs are not all
# here: the bare directory is still a usable artifact.
build_installer() {
    crypto_dll=$1

    if [ -z "$crypto_dll" ]; then
        echo "package: no libcrypto DLL copied; skipping the installer" >&2
        return 0
    fi

    if ! iscc=$(find_iscc); then
        echo "package: Inno Setup (ISCC.exe) not found; dist/ is runnable, not installable" >&2
        return 0
    fi

    # `VersionInfoVersion` must be four numbers, and a prerelease suffix has
    # none: `0.2.0-beta.1` becomes `0.2.0.0`.
    numeric_version=$(printf '%s' "$version" | sed -E 's/^([0-9]+\.[0-9]+\.[0-9]+).*/\1.0/')

    # Git-Bash/MSYS rewrites an argument that starts with `/` into a Windows
    # path, which would turn `/Q` and every `/D...` define into a "filename"
    # for ISCC ("may not specify more than one script filename"). The paths it
    # is given are already converted with `winpath`, so suppressing that
    # rewrite for this one call is safe.
    MSYS2_ARG_CONV_EXCL='*' MSYS_NO_PATHCONV=1 "$iscc" /Q \
        "/DVersion=$version" \
        "/DVersionNumeric=$numeric_version" \
        "/DSourceDir=$(winpath "$out")" \
        "/DOutputDir=$(winpath "$out")" \
        "/DIconFile=$(winpath "$root/crates/app/icons/$name.ico")" \
        "/DArchitecture=$relarch" \
        "/DAppId=$app_id" \
        "/DCryptoDll=$crypto_dll" \
        "$(winpath "$root/crates/app/packaging/$name.iss")"
}

# --- macOS: a .app bundle, and a .dmg around it ------------------------------
#
# `Info.plist` and the `.icns` are the branding; the Dock and the application
# menu read nothing else. Not signed or notarized here — an unsigned bundle runs
# locally (arm64 gets an ad-hoc linker signature) but needs right-click -> Open
# after download.
#
# The `.dmg` is the same bundle plus an `Applications` symlink, which is the
# drag-to-install idiom: the Finder shows the app next to the folder to drop it
# in. A plain `-srcfolder` of the bundle alone would read as an archive of a
# folder, not an install.
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

    # `hdiutil` ships only with macOS, so its presence is the whole guard.
    if command -v hdiutil >/dev/null 2>&1; then
        dmg="$out/$name-$version-macos-$relarch.dmg"
        stage="$out/dmg"

        # A scratch directory under `dist/` rather than `mktemp`, so the layout
        # is inspectable if a build fails midway. Removed below: the staging
        # tree must not be left beside the artifacts.
        rm -rf "$stage"
        mkdir -p "$stage"
        cp -R "$app" "$stage/"
        ln -s /Applications "$stage/Applications"

        # UDZO is zlib-compressed and read-only: the format every distributed
        # `.dmg` is in, and the only one an unsigned image mounts cleanly as.
        hdiutil create -quiet -volname "$name" -srcfolder "$stage" -ov \
            -format UDZO "$dmg"
        rm -rf "$stage"

        echo "packaged into $dmg" >&2
    else
        echo "package: hdiutil not found; skipping the .dmg" >&2
    fi

    echo "packaged into $app" >&2
}

# --- Linux: an install-prefix tarball, and a .deb over it --------------------
#
# The window's `app_id` is what ties the running app to the icon and the
# `.desktop` entry, so all three carry `$app_id`: the entry's file name,
# `StartupWMClass`, and the hicolor icon file name.
#
# Two artifacts from one tree: the tarball is for people who unpack things
# themselves, the `.deb` is for `apt`. They share the prefix so the layout a
# user sees cannot drift between them.
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

    # The tarball is what a user unpacks, so it keeps the prefix shape (`bin/`,
    # `share/`) unchanged; the `.deb` is built from a copy under `usr/` below.
    ( cd "$out" && tar czf "$name-$version-linux-$(uname -m).tar.gz" "$(basename "$prefix")" )

    build_deb "$prefix"

    echo "packaged into $prefix (.tar.gz beside it, plus a .deb where dpkg-deb exists)" >&2
}

# Debian's `Architecture:` spelling differs from the release's file spelling for
# the same machine: dpkg says `amd64`, not `x86_64`, and `arm64`, not `aarch64`.
# Keeping both in one place means the control field and the file name cannot
# disagree about which build this is.
deb_arch() {
    case "$1" in
        x86_64) echo amd64 ;;
        aarch64) echo arm64 ;;
        *) echo "$1" ;;
    esac
}

# Compile the prefix into `$out/<name>-<v>-linux-<relarch>.deb`. Skipped, with
# the reason, where `dpkg-deb` is missing — the tarball is still usable there.
build_deb() {
    prefix=$1

    if ! command -v dpkg-deb >/dev/null 2>&1; then
        echo "package: dpkg-deb not found; skipping the .deb" >&2
        return 0
    fi

    # dpkg unpacks the tree at `/`, so the prefix has to be relocated under
    # `usr/` first: `/share` is not a directory on any distro, and `/bin` is
    # itself a usrmerge symlink on every system dpkg runs on. Staged beside the
    # prefix rather than inside it, so the tarball can never pick up `DEBIAN/`
    # or the `usr/` copy.
    debroot="$out/$name-$version-linux-deb"
    rm -rf "$debroot"
    mkdir -p "$debroot/usr" "$debroot/DEBIAN"
    cp -R "$prefix/bin" "$prefix/share" "$debroot/usr/"

    # KiB, the unit `Installed-Size` is defined in, counted before `DEBIAN/`
    # exists so the metadata is not part of its own size.
    installed_size=$(du -sk "$debroot/usr" | cut -f1)

    # `Depends` lists what the GPUI binary resolves at load time — the X11,
    # Wayland and font stack it links against rather than bundles. No glibc
    # floor is pinned: the script builds on whatever host it runs on, and a
    # guessed minimum would either be wrong or block working installs.
    cat > "$debroot/DEBIAN/control" <<CONTROL
Package: $name
Version: $version
Section: utils
Priority: optional
Architecture: $(deb_arch "$relarch")
Installed-Size: $installed_size
Maintainer: restbook
Depends: libc6, libxkbcommon0, libxkbcommon-x11-0, libwayland-client0, libfontconfig1, libfreetype6, libzstd1, libx11-xcb1, libxcb1, libxcb-render0, libxcb-shape0, libxcb-xfixes0, libxcb-randr0, libxcb-composite0, libxcb-damage0, libxcb-present0, libxcb-sync1
Description: Local-first HTTP client with encrypted history
 A desktop REST client whose request history and workspaces live on the
 local disk, encrypted at rest.
CONTROL

    # Both scripts run on every install and removal and only ever regenerate a
    # cache, so they are idempotent, cheap, and never fail the transaction: a
    # missing `desktop-file-utils` or `gtk-update-icon-cache` leaves the caches
    # to be rebuilt by something else instead of aborting `dpkg`.
    cat > "$debroot/DEBIAN/postinst" <<'POSTINST'
#!/bin/sh
command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database -q || true
command -v gtk-update-icon-cache >/dev/null 2>&1 && gtk-update-icon-cache --quiet >/dev/null 2>&1 || true
exit 0
POSTINST

    cat > "$debroot/DEBIAN/postrm" <<'POSTRM'
#!/bin/sh
command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database -q || true
command -v gtk-update-icon-cache >/dev/null 2>&1 && gtk-update-icon-cache --quiet >/dev/null 2>&1 || true
exit 0
POSTRM

    # dpkg runs maintainer scripts as programs, and a freshly written file has
    # no execute bit under a normal umask, so set it explicitly.
    chmod 755 "$debroot/DEBIAN/postinst" "$debroot/DEBIAN/postrm"

    deb="$out/$name-$version-linux-$relarch.deb"

    # `--root-owner-group`: the package belongs to root whoever built it, so a
    # CI user's uid never leaks into files under `/usr`.
    dpkg-deb --build --root-owner-group "$debroot" "$deb"

    rm -rf "$debroot"

    echo "packaged into $deb" >&2
}

# Dispatch after every function is defined: `sh` reads a script top to bottom,
# so a `case` above the definitions would not find them.
case "$os" in
    windows) package_windows ;;
    macos) package_macos ;;
    linux) package_linux ;;
esac
