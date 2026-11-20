; restbook's Windows installer.
;
; A bare `restbook-app.exe` imports `libcrypto-*` at load time, so it only runs
; next to the DLL that shipped with it, and it gets a generic taskbar identity.
; This script wraps both files into one self-installing executable and gives it
; the identity `branding::APP_ID` declares.
;
; `scripts/package.sh` compiles it with Inno Setup's `ISCC.exe` and passes what
; cannot be read from this file:
;
;   /DVersion         app version, from `crates/app/Cargo.toml`
;   /DVersionNumeric  the same, as a four-field number (`0.2.0-beta.1` -> 0.2.0.0)
;   /DSourceDir       `dist/`, holding the exe and the crypto DLL
;   /DOutputDir       where the setup executable lands
;   /DIconFile        `crates/app/icons/restbook.ico`
;   /DArchitecture    `x86_64` / `aarch64`; part of the artifact name
;   /DAppId           `branding::APP_ID`, the taskbar identity
;   /DCryptoDll       basename of the OpenSSL DLL the exe imports
;
; The shortcut's `AppUserModelID` must equal `branding::APP_ID`: Windows keys
; taskbar grouping, and therefore the pinned icon, on that string, and a
; mismatch leaves a running window ungrouped and its icon generic.

#ifndef Version
  #error Compile with ISCC /DVersion=<x.y.z>; see scripts/package.sh
#endif
#ifndef VersionNumeric
  #error Compile with ISCC /DVersionNumeric=<a.b.c.d>; see scripts/package.sh
#endif
#ifndef SourceDir
  #error Compile with ISCC /DSourceDir=<dist>; see scripts/package.sh
#endif
#ifndef OutputDir
  #error Compile with ISCC /DOutputDir=<out>; see scripts/package.sh
#endif
#ifndef IconFile
  #error Compile with ISCC /DIconFile=<restbook.ico>; see scripts/package.sh
#endif
#ifndef Architecture
  #define Architecture "x86_64"
#endif
#ifndef AppId
  #define AppId "com.restbook.restbook"
#endif
; `CryptoDll` has no default on purpose: a build that links OpenSSL statically (see
; `sqlcipher-vendored`) has no DLL to ship, and the packaging script then passes
; no define at all. The `[Files]` entry below disappears with it.

[Setup]
; The setup's own identity, unrelated to the app id below. It has to stay
; constant across versions so an upgrade replaces the previous install instead
; of appearing as a second product in Add or remove programs.
AppId={{8F3C1A24-6B7D-4F0E-9A1C-2D5E7B9C4A10}
AppName=restbook
AppVerName=restbook {#Version}
AppVersion={#Version}
VersionInfoVersion={#VersionNumeric}
AppPublisher=restbook
; Per-user by design: the app keeps its history in `%APPDATA%\restbook`, so
; nothing needs an administrator, and an unsigned installer never raises a UAC
; prompt it cannot back with a signature. `{autopf}` under `lowest` is
; `%LOCALAPPDATA%\Programs`.
PrivilegesRequired=lowest
DefaultDirName={autopf}\restbook
DefaultGroupName=restbook
DisableProgramGroupPage=yes
AllowNoIcons=yes
SetupMutex=restbook-setup
SetupIconFile={#IconFile}
UninstallDisplayIcon={app}\restbook-app.exe
SourceDir={#SourceDir}
OutputDir={#OutputDir}
OutputBaseFilename=restbook-{#Version}-windows-{#Architecture}
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
; A running copy holds the exe open; Restart Manager closes it before the files
; are replaced, and the app is relaunched afterwards by [Run] if it was started
; from the wizard.
CloseApplications=yes
RestartApplications=no
MinVersion=10.0.17763

#if Architecture == "aarch64"
ArchitecturesAllowed=arm64
ArchitecturesInstallIn64BitMode=arm64
#else
ArchitecturesAllowed=x64compatible and not arm64
ArchitecturesInstallIn64BitMode=x64compatible and not arm64
#endif

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "restbook-app.exe"; DestDir: "{app}"; Flags: ignoreversion
; The DLL's name follows the OpenSSL major version, so it is passed in rather
; than spelled out here — and it is absent when the build linked OpenSSL
; statically, where the exe needs nothing beside it.
#ifdef CryptoDll
Source: "{#CryptoDll}"; DestDir: "{app}"; Flags: ignoreversion
#endif

; An upgrade replaces the files it knows about and leaves everything else, so a
; DLL an earlier version shipped and this one does not stays in `{app}` forever
; — which is how an OpenSSL major bump ends up with `libcrypto-3-x64.dll` and
; `libcrypto-4-x64.dll` side by side, one of them dead. Removing the pattern
; here is safe because this section runs before `[Files]`, so the DLL the new
; build imports is written back afterwards. An install that ships none (a
; static build) simply ends up without one.
[InstallDelete]
Type: files; Name: "{app}\libcrypto-*.dll"

[Icons]
Name: "{group}\restbook"; Filename: "{app}\restbook-app.exe"; AppUserModelID: "{#AppId}"
Name: "{autodesktop}\restbook"; Filename: "{app}\restbook-app.exe"; Tasks: desktopicon; AppUserModelID: "{#AppId}"

[Run]
Filename: "{app}\restbook-app.exe"; Description: "{cm:LaunchProgram,restbook}"; Flags: nowait postinstall skipifsilent
