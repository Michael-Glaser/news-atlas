# ATLAS -- the Windows helper behind ATLAS.bat. Double-click ATLAS.bat, not this file.
#
# What it does, in order, and nothing else:
#
#   1. checks that this PC has the Rust tools ATLAS is built with (once);
#   2. builds ATLAS if this copy has never been built -- a build step, on this machine;
#   3. runs `atlas setup --install`, which fetches the model file (the one download, and
#      it can be paused and resumed) and builds the model server;
#   4. opens the app.
#
# It never downloads anything itself: the only thing here that fetches bytes is
# `atlas-setup.exe --install`, and only after it has printed the address it will use. It
# opens no port. Every message is one plain sentence; no colour, no symbols.
$ErrorActionPreference = "Continue"
$ROOT = Split-Path -Parent $MyInvocation.MyCommand.Path
Set-Location $ROOT

$MANIFEST = Join-Path $ROOT "rust\Cargo.toml"
$RELEASE  = Join-Path $ROOT "rust\target\release"
$DESKTOP  = Join-Path $RELEASE "atlas-desktop.exe"
$SETUP    = Join-Path $RELEASE "atlas-setup.exe"
$LAUNCHER = Join-Path $RELEASE "atlas.exe"

function Say($line) { Write-Host $line }
function Stop-Here($line) {
    Write-Host ""
    Write-Host "ATLAS could not start: $line"
    exit 1
}

Say ""
Say "ATLAS - getting this PC ready."
Say ""

# A folder copied from a network share or another account is read-only here, and every
# later step would fail with a message from Windows instead of from ATLAS.
$probe = Join-Path $ROOT ".atlas-write-test"
try {
    Set-Content -LiteralPath $probe -Value "x" -ErrorAction Stop
    Remove-Item -LiteralPath $probe -ErrorAction SilentlyContinue
} catch {
    Stop-Here "ATLAS cannot write into this folder ($ROOT). Move the whole ATLAS folder into your own user folder, for example Documents, and start ATLAS.bat again."
}

# --- 1. the Rust tools ---------------------------------------------------------------
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    Say "One thing is missing: the Rust tools ATLAS is built with."
    Say "Go to https://rustup.rs, follow the single step there,"
    Say "then double-click ATLAS.bat again."
    Stop-Here "the Rust tools are not installed yet."
}

# --- 2. build, if this copy has never been built -------------------------------------
if ((-not (Test-Path $DESKTOP)) -or (-not (Test-Path $SETUP)) -or (-not (Test-Path $LAUNCHER))) {
    Say "Building ATLAS on this PC. This happens once and takes a few minutes."
    & cargo build --release --offline --locked --manifest-path $MANIFEST
    if ($LASTEXITCODE -ne 0) {
        Say "First attempt could not finish: this PC has no copy of the building blocks yet."
        Say "Trying once more, downloading them this one time."
        & cargo build --release --locked --manifest-path $MANIFEST
        if ($LASTEXITCODE -ne 0) {
            Stop-Here "ATLAS could not be built. The lines above are the reasons. The usual causes are no internet connection while the building blocks are fetched, and a copy that lives in a read-only folder. Connect to the internet and start ATLAS.bat again; if it still stops here, run .\setup.ps1 in a PowerShell window and send the last ten lines."
        }
    }
    Say "Built."
    Say ""
}

# --- 3. the licence, the model, the model server -------------------------------------
#
# `atlas-setup.exe --install` starts with the licence: the Apache-2.0 terms, what they do
# not cover, and one question. Only a yes allows the one download (about 6.5 GB into
# the models folder, from huggingface.co) and the model server built from the copy of llama.cpp
# that ships beside this file. Any other answer stops here, and nothing is fetched.
Say "Next: the licence question, then one download of about 6.5 GB into models."
Say "If the connection drops, start this file again - it carries on from where it stopped."
if (-not (Test-Path $SETUP)) {
    Stop-Here "the setup program is missing from rust\target\release. Run .\setup.ps1 in a PowerShell window."
}
& $SETUP --install
if ($LASTEXITCODE -ne 0) {
    Stop-Here "the setup step above says what is missing."
}

# --- 4. the app ---------------------------------------------------------------------
Say ""
Say "Opening ATLAS. The first time, it shows the licence in full: tick the line under it"
Say "and the workspace opens. Close its window to stop ATLAS."
Say ""
& $LAUNCHER start
if ($LASTEXITCODE -ne 0) {
    Stop-Here "the app closed with an error; the lines above say why."
}
