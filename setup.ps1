# ATLAS setup.ps1 -- provision a fresh machine, then check it (Windows, PowerShell).
#
#   powershell -ExecutionPolicy Bypass -File .\setup.ps1
#   powershell -ExecutionPolicy Bypass -File .\setup.ps1 -Provision
#   powershell -ExecutionPolicy Bypass -File .\setup.ps1 --help
#
# Five steps, and the interesting ones are 2, 3 and 4:
#
#   1. Rust         the toolchain must be there; a missing one is a sentence
#   2. build        cargo build --release --offline --locked (once with the network if the
#                   crate cache is empty -- a BUILD step, never a run-time fetch), then
#                   the check that the product's face was produced:
#                   rust\target\release\atlas-desktop.exe, the native window
#   3. atlas-setup  the pieces cargo does not build: the vendored llama.cpp tree,
#                   bin\llama-server.exe, the pinned model file. One line each, each
#                   naming the subcommand that fixes it. They are for chat: the window
#                   starts without them. -Provision runs them; without it nothing large
#                   is fetched and nothing is built. The one download is the 6.5 GB
#                   model, from the URL printed first, over https, verified against a
#                   pinned sha256 before use.
#   4. the window   the two files the window actually opens: data\atlas.db and the
#                   vendored map outlines. Read-only in both directions -- this step
#                   binds no port and probes none.
#   5. data\ privacy  the ACL, which is how a directory is made private on Windows.
#                   `atlas-platform` reports the *inherited* ACL honestly and does not
#                   pretend a chmod happened, so cutting inheritance is this one step,
#                   run once, and it says so if it is refused.
#
# The app itself never fetches: `atlas-setup` is a separate binary that no crate links,
# and the window never spawns it. See docs\OPERATIONS.md.
# Idempotent: run it again after you provisioned something.
param([switch]$Provision, [switch]$Help)

$ErrorActionPreference = "Continue"
$ROOT = Split-Path -Parent $MyInvocation.MyCommand.Path
Set-Location $ROOT

$MANIFEST = Join-Path $ROOT "rust\Cargo.toml"
$RELEASE  = Join-Path $ROOT "rust\target\release"
$DESKTOP  = Join-Path $RELEASE "atlas-desktop.exe"
$SETUP    = Join-Path $RELEASE "atlas-setup.exe"

function Log($m)  { Write-Host "[setup] $m" }
function Warn($m) { Write-Host "[setup] $m" }
function Die($m)  { Write-Host "[setup] $m"; exit 1 }

# The header comment is the help, read from this file up to the first statement, so the
# two scripts answer --help with the same text they are documented by. setup.sh has done
# this since it was written; this script had no argument check at all, so a person typing
# .\setup.ps1 --help got a full build instead of an explanation -- the most expensive
# possible answer to a question.
function Show-Help {
    foreach ($line in Get-Content -LiteralPath $PSCommandPath) {
        if ($line -notmatch '^#') { break }
        Write-Host ($line -replace '^# ?', '')
    }
}

if ($Help) {
    Show-Help
    exit 0
}

if ($args.Count -gt 0) {
    $first = [string]$args[0]
    # PowerShell binds a single-dash parameter before this runs, so `-Help` and
    # `-Provision` are declared in the param block; `--help` and `--provision` are
    # positional values and land here, which is how everyone who has used a POSIX shell
    # -- and every document in this repository -- spells them. `--provision` used to be
    # refused while setup.sh accepted it: the same word meant two things on two
    # platforms, and the person on Windows was the one told they were wrong.
    if ($first -eq "-h" -or $first -eq "--help") {
        Show-Help
        exit 0
    }
    if ($first -eq "--provision" -or $first -eq "-provision") {
        # The param block typed it as a switch; a bool coerces to one, and a switch in
        # an if is "was this asked for".
        $Provision = $true
        if ($args.Count -gt 1) { Die "unknown argument $([string]$args[1]) (only -Provision and --help)" }
    } else {
        Die "unknown argument $first (only -Provision and --help)"
    }
}

# ---------- 1. Rust ----------------------------------------------------------
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    Die "Rust is missing. Install it once from https://rustup.rs, open a new terminal, then run .\setup.ps1 again."
}
$cargoVersion = (& cargo --version 2>&1 | Out-String).Trim()
$rustcVersion = (& rustc --version 2>&1 | Out-String).Trim()
Log "$cargoVersion / $rustcVersion"

# ---------- 2. build ---------------------------------------------------------
Log "cargo build --release --offline --locked --manifest-path rust\Cargo.toml"
& cargo build --release --offline --locked --manifest-path $MANIFEST
if ($LASTEXITCODE -ne 0) {
    Warn "the offline build failed -- on a fresh machine the crate cache is empty."
    Warn "retrying once with the network: dependencies are fetched at BUILD time,"
    Warn "never at run time. Nothing else in ATLAS reaches the network."
    & cargo build --release --locked --manifest-path $MANIFEST
    if ($LASTEXITCODE -ne 0) { Die "cargo build failed -- ATLAS cannot run without its workspace." }
}
if (-not (Test-Path $DESKTOP)) {
    Die "rust\target\release\atlas-desktop.exe was not produced -- the workspace is not what this script expects."
}
if (-not (Test-Path $SETUP)) {
    Die "rust\target\release\atlas-setup.exe was not produced -- the workspace is not what this script expects."
}
Log "built: rust\target\release (atlas-desktop, the engines and the tools)"

# ---------- 3. atlas-setup: the pieces cargo does not build -------------------
# `atlas-setup check` is the question; `atlas-setup all --yes` is the action. The check
# runs first either way, so the run below never starts on a guess. Its report is its
# own: the lines above are what it checks, not what this script claims.
& $SETUP check --root $ROOT
if ($LASTEXITCODE -eq 0) {
    Log "atlas-setup: every piece it checks is present."
} elseif ($Provision) {
    Log "-Provision: running atlas-setup all --yes -- it prints each step it takes"
    & $SETUP all --root $ROOT --yes
    if ($LASTEXITCODE -ne 0) {
        Die "atlas-setup failed -- the line above names the piece and the reason. Fix it and re-run .\setup.ps1 -Provision."
    }
} else {
    Warn "the pieces above are missing. Nothing was fetched or built."
    Warn "chat needs bin\llama-server.exe and the pinned model; the window itself needs neither."
    Warn "run .\setup.ps1 -Provision, or run the subcommand each line named (atlas-setup.exe is in rust\target\release)."
}

# ---------- 4. the window's own files ---------------------------------------
# No `atlas doctor` here any more: the launcher's doctor probes the model server's
# loopback port before every boot, and this build opens no port at all -- the probe
# connects, it never binds, but a setup step has no business asking at all. What the
# window opens is two files, so those are what this step reads -- and it reads them, it
# does not touch them.
$db = Join-Path $ROOT "data\atlas.db"
if (Test-Path $db) {
    $mb = [math]::Round(((Get-Item $db).Length / 1MB), 1)
    Log "archive: data\atlas.db ($mb MB) -- the window reads it read-only"
} else {
    Warn "archive: data\atlas.db is not there yet -- the window opens with an empty archive and says so in its status row."
    Warn "it is written by the ingest pipeline, never by the window."
}
$outlines = Join-Path $ROOT "public\vendor\ne_50m_countries.geojson"
if (Test-Path $outlines) {
    Log "map outlines: public\vendor\ne_50m_countries.geojson"
} else {
    Warn "map outlines: public\vendor\ne_50m_countries.geojson is missing -- the map view draws the graticule and the pins, no coastlines."
}

# ---------- 5. data\ ACL ---------------------------------------------------
# The archive, the logs and config.json live in data\. Windows has no POSIX modes,
# so the answer is an ACL: cut inheritance and keep this account only.
New-Item -ItemType Directory -Force -Path "data" | Out-Null
# The account is named as `domain\user` (on a workgroup machine the "domain" is the
# computer name). A bare user name is resolved against the local machine first, so on a
# domain-joined machine it can name a different account than the one running this
# script -- which would tighten the ACL for somebody else and leave the real account
# outside it. USERDOMAIN is unset in a few minimal environments, hence the fallback.
$me = if ($env:USERDOMAIN) { "$($env:USERDOMAIN)\$($env:USERNAME)" } else { $env:USERNAME }
$aclOut = (icacls "data" /inheritance:r /grant:r "${me}:(OI)(CI)F" 2>&1 | Out-String).Trim()
if ($LASTEXITCODE -ne 0) {
    Warn "could not tighten the ACL on data\ -- inherited permissions are unchanged."
    if ($aclOut) { Warn "icacls said: $aclOut" }
    Warn "manual step: right-click data\ > Properties > Security and remove other accounts."
} else {
    Log "data\ ACL: $me only (an ACL, not a mode: Windows has no 0600)"
}

Log "ready. open the window with .\run.ps1 (or: rust\target\release\atlas-desktop.exe)"
