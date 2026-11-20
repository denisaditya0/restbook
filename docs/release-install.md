<!-- restbook-install -->
## Install

These builds are unsigned, so each OS shows a first-run warning — see the
per-platform note below its artifact.

- **Windows** — download `restbook-<version>-windows-x86_64.exe` and run it. It
  installs per-user: `restbook-app.exe` — one file, OpenSSL linked in —
  a Start-menu entry, an optional desktop shortcut, and an uninstaller. No admin
  rights needed. SmartScreen warns about an unknown publisher; choose
  *More info → Run anyway*.
- **macOS** — download `restbook-<version>-macos-<arch>.dmg`, open it, and drag
  restbook to Applications. It is ad-hoc signed but not notarized, so the first
  launch needs right-click → *Open* (or *System Settings → Privacy & Security →
  Open Anyway*).
- **Linux** — download `restbook-<version>-linux-x86_64.deb` and install with
  `sudo apt install ./restbook-<version>-linux-x86_64.deb`. A
  `…-linux-x86_64-portable.tar.gz` prefix tree is provided if you would rather
  not install.

Each asset's SHA-256 is listed in the release's `latest.json`; compare it if you
want to be sure a download arrived intact.
<!-- /restbook-install -->
