#!/usr/bin/env sh
# Run cargo with a development environment this machine can build.
#
# The `sqlcipher` feature is on by default in `crates/app` (and opt-in in the
# engine), and `libsqlite3-sys` links libcrypto: its build script reads
# `OPENSSL_DIR` and panics without it. Rather than pinning one machine's path,
# detect an OpenSSL dev tree here and forward every argument to cargo:
#
#   scripts/dev.sh check
#   cd crates/app && ../../scripts/dev.sh run
#   scripts/dev.sh --print-openssl-dir   # what a packaging script should copy
#
# An explicit `OPENSSL_DIR` is never overwritten. If nothing is found the script
# still runs cargo, which then reports what is missing.
set -eu

if [ -z "${OPENSSL_DIR:-}" ]; then
    detected=$(
        # A MinGW toolchain usually keeps a sibling `opt` with OpenSSL headers.
        # The compiler's own tree comes first: a `pkg-config` earlier on PATH
        # can belong to another toolchain (Strawberry Perl ships one) and name
        # an OpenSSL this compiler does not link.
        for prefix in \
            "${MINGW_PREFIX:-/nonexistent}" \
            "$(dirname "$(command -v gcc 2>/dev/null || echo /nonexistent/x)")/../opt" \
            "${SCOOP:-$HOME/scoop}/apps/mingw/current/opt"; do
            if [ -f "$prefix/include/openssl/ssl.h" ]; then
                echo "$prefix"
                break
            fi
        done

        # A pkg-config .pc file names the prefix, when one is installed.
        if command -v pkg-config >/dev/null 2>&1; then
            pkg-config --variable=prefix openssl 2>/dev/null || true
        fi

        # The usual package-manager prefixes, in case the above found nothing.
        for prefix in \
            "${SCOOP:-$HOME/scoop}/apps/openssl/current" \
            /mingw64 /usr/local /usr \
            /opt/homebrew/opt/openssl /usr/local/opt/openssl; do
            if [ -f "$prefix/include/openssl/ssl.h" ]; then
                echo "$prefix"
                break
            fi
        done
    )

    detected=$(printf '%s\n' "$detected" | head -n 1)
    if [ -n "$detected" ]; then
        # Keep the shell form for PATH and give the build scripts a native one:
        # `cygpath -m` yields `C:/...`, which a `:`-separated PATH cannot hold.
        openssl_prefix=$detected
        if command -v cygpath >/dev/null 2>&1; then
            OPENSSL_DIR=$(cygpath -m "$openssl_prefix")
        else
            OPENSSL_DIR=$openssl_prefix
        fi
        export OPENSSL_DIR
        echo "dev: OPENSSL_DIR=$OPENSSL_DIR" >&2
    fi
fi

openssl_prefix=${openssl_prefix:-${OPENSSL_DIR:-}}
if [ -n "$openssl_prefix" ] && command -v cygpath >/dev/null 2>&1; then
    openssl_prefix=$(cygpath -u "$openssl_prefix" 2>/dev/null || printf '%s' "$openssl_prefix")
fi

# libcrypto is linked dynamically, so the loader needs the DLL (or .so) that
# matches it at *run* time. `cargo run` and `cargo test` do not add the OpenSSL
# prefix to PATH, and another toolchain's copy may win; put this one first.
if [ -n "$openssl_prefix" ] && [ -d "$openssl_prefix/bin" ]; then
    PATH="$openssl_prefix/bin:$PATH"
    export PATH
fi

if [ "${1:-}" = "--print-openssl-dir" ]; then
    printf '%s\n' "${OPENSSL_DIR:-}"
    exit 0
fi

exec cargo "$@"
