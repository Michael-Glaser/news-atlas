# ATLAS run.ps1 -- open the native window (Windows, PowerShell).
#
#   powershell -ExecutionPolicy Bypass -File .\run.ps1
#   powershell -ExecutionPolicy Bypass -File .\run.ps1 --help
#
# The product is one Rust binary: rust\target\release\atlas-desktop.exe, an egui
# window that reads data\atlas.db read-only. It opens no socket and serves no page --
# there is no port to wait for and no address to open, which is why none of that is
# here any more.
#
# It does not start the model server either. Chat is the one view that needs
# bin\llama-server.exe, and the window's status row says whether it is there instead
# of guessing; the model server and the model file are provisioned by setup.ps1.
#
# So this script only does what a shell does better than the binary: make sure that
# binary exists, then start it. Nothing is downloaded or installed at run time.
$ErrorActionPreference = "Continue"
$ROOT = Split-Path -Parent $MyInvocation.MyCommand.Path
Set-Location $ROOT

$RELEASE = Join-Path $ROOT "rust\target\release"
$DESKTOP = Join-Path $RELEASE "atlas-desktop.exe"
$MANIFEST = Join-Path $ROOT "rust\Cargo.toml"

function Say($m) { Write-Host "[run] $m" }
function Die($m) { Write-Host "[run] $m"; exit 1 }

# The window parses no arguments: it finds the project from its own path and reads
# data\atlas.db. Forwarding an argument it would ignore is a promise this script
# cannot keep, so an argument is refused with the sentence that says so.
if ($args.Count -gt 0) {
    if ($args[0] -eq "-h" -or $args[0] -eq "--help") {
        Write-Host "ATLAS run.ps1 -- open the native window"
        Write-Host ""
        Write-Host "  .\run.ps1          build rust\target\release\atlas-desktop.exe if it is missing, then open it"
        Write-Host "  .\run.ps1 --help   this text"
        Write-Host ""
        Write-Host "The window takes no arguments. It reads data\atlas.db, opens no socket and"
        Write-Host "serves no page: the web frontend and the port it served are retired."
        exit 0
    }
    Die "the native window takes no arguments (it is not `atlas start` any more): .\run.ps1"
}

if (-not (Test-Path $DESKTOP)) {
    if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
        Die "Rust is missing. Install it once from https://rustup.rs, open a new terminal, then run .\setup.ps1"
    }
    Say "the window is not built: cargo build --release --offline --locked -p atlas-desktop"
    # `--locked` on both attempts: the retry may fill an empty crate cache, it may
    # never re-resolve (rust\build.sh has the long version of why).
    & cargo build --release --offline --locked -p atlas-desktop --manifest-path $MANIFEST
    if ($LASTEXITCODE -ne 0) {
        Say "the offline build failed -- on a fresh machine the crate cache is empty."
        Say "retrying once with the network: crates are fetched at BUILD time only."
        & cargo build --release --locked -p atlas-desktop --manifest-path $MANIFEST
        if ($LASTEXITCODE -ne 0) { Die "cargo build failed -- run .\setup.ps1 for the full report." }
    }
    if (-not (Test-Path $DESKTOP)) { Die "rust\target\release\atlas-desktop.exe is still missing -- run .\setup.ps1" }
}

# The one file the window reads that cargo does not produce. Its absence is not a
# refusal -- the window opens, shows an empty archive and says so in its status row --
# so this is a line, not a stop.
if (-not (Test-Path (Join-Path $ROOT "data\atlas.db"))) {
    Say "data\atlas.db is not there yet: the window opens with an empty archive and says so."
}

# Chat is the one view that dials the model server, and this script starts nothing but
# the window: the two pieces chat needs are named here rather than started here, and the
# rest of the window works without them. No port is probed -- a probe would open the
# socket this script promises not to open.
$server = Join-Path $ROOT "bin\llama-server.exe"
$model = @(Get-ChildItem -Path (Join-Path $ROOT "models") -Filter *.gguf -ErrorAction SilentlyContinue)
if ((Test-Path $server) -and ($model.Count -gt 0)) {
    Say "chat needs the model server running: rust\target\release\atlas.exe start starts it ahead of the window."
} else {
    Say "chat will report a missing model server: bin\llama-server.exe or the model file under models\ is not there."
    Say ".\setup.ps1 -Provision builds the first and fetches the second; the window opens without them."
}

& $DESKTOP
exit $LASTEXITCODE
