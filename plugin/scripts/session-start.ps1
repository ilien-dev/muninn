# SessionStart bootstrap for the Codex plugin on Windows, where Codex runs hook commands
# through cmd.exe and there is no sh to run scripts/session-start. It does the same job:
# the first session after an install or an update downloads the release binary that
# matches this plugin's version into the plugin's own bin\, verifies its SHA-256 against
# the release's checksums.txt, and keeps a copy in %USERPROFILE%\.local\bin for a person
# typing `muninn` in a terminal. It never runs the binary: the hook command runs it after
# this script succeeds, so the hook's stdin reaches muninn.exe untouched. A failed
# download exits 1 with the reason on stderr, the hook command skips the binary and still
# exits 0, and the next session tries again.
$ErrorActionPreference = 'Stop'
$root = if ($env:PLUGIN_ROOT) { $env:PLUGIN_ROOT } else { Split-Path -Parent $PSScriptRoot }
$binDir = Join-Path $root 'bin'
$bin = Join-Path $binDir 'muninn.exe'

if (-not (Test-Path -LiteralPath $bin)) {
  try {
    $manifest = Join-Path $root '.codex-plugin\plugin.json'
    if (-not (Test-Path -LiteralPath $manifest)) { $manifest = Join-Path $root '.claude-plugin\plugin.json' }
    $version = if ($env:MUNINN_VERSION) { $env:MUNINN_VERSION } else {
      (Get-Content -Raw -LiteralPath $manifest | ConvertFrom-Json).version
    }
    $owner = if ($env:MUNINN_REPO) { $env:MUNINN_REPO } else { 'ilien-dev/muninn' }
    $repo = if ($env:MUNINN_RELEASE_URL) { $env:MUNINN_RELEASE_URL } else {
      "https://github.com/$owner/releases/download/v$version"
    }
    # Windows PowerShell 5.1 still offers TLS 1.0 first; GitHub refuses it
    [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
    $asset = 'muninn-x86_64-pc-windows-msvc.exe'
    $tmp = Join-Path ([IO.Path]::GetTempPath()) ("muninn-" + [Guid]::NewGuid())
    New-Item -ItemType Directory -Path $tmp | Out-Null
    try {
      $ProgressPreference = 'SilentlyContinue'  # the progress bar slows 5.1 downloads tenfold
      # MUNINN_RELEASE_URL may name a local mirror (file:///C:/...), which the install test uses
      function Fetch($name, $out) {
        $uri = [Uri]"$repo/$name"
        if ($uri.IsFile) { Copy-Item -LiteralPath $uri.LocalPath -Destination $out }
        else { Invoke-WebRequest -UseBasicParsing -Uri $uri -OutFile $out }
      }
      Fetch $asset (Join-Path $tmp 'muninn.exe')
      Fetch 'checksums.txt' (Join-Path $tmp 'checksums.txt')
      $line = Get-Content -LiteralPath (Join-Path $tmp 'checksums.txt') | Where-Object { $_ -match " $([regex]::Escape($asset))$" } | Select-Object -First 1
      if (-not $line) { throw "no checksum published for $asset" }
      $expected = ($line -split '\s+')[0].ToLowerInvariant()
      $actual = (Get-FileHash -Algorithm SHA256 -LiteralPath (Join-Path $tmp 'muninn.exe')).Hash.ToLowerInvariant()
      if ($expected -ne $actual) { throw "checksum mismatch for $asset" }
      New-Item -ItemType Directory -Force -Path $binDir | Out-Null
      # copy next to the target, then rename over it: a hook starting meanwhile sees the old
      # binary or the new one, never a half-written file
      $part = Join-Path $binDir ".muninn.exe.$PID"
      Copy-Item -LiteralPath (Join-Path $tmp 'muninn.exe') -Destination $part -Force
      Move-Item -LiteralPath $part -Destination $bin -Force
      [Console]::Error.WriteLine("installed $bin ($version, x86_64-pc-windows-msvc)")
    } finally {
      Remove-Item -Recurse -Force -LiteralPath $tmp -ErrorAction SilentlyContinue
    }
  } catch {
    [Console]::Error.WriteLine("muninn: could not download muninn $version into ${binDir}: $($_.Exception.Message); memory is off for this session and the next one will retry")
    exit 1
  }
}

# A copy for the terminal, marked as ours so a muninn.exe someone installed by hand is left
# alone, and refreshed when the plugin's binary is newer (an update downloads a new one).
try {
  $home_ = if ($env:MUNINN_BIN_DIR) { $env:MUNINN_BIN_DIR } else { Join-Path $env:USERPROFILE '.local\bin' }
  $link = Join-Path $home_ 'muninn.exe'
  $mark = Join-Path $home_ 'muninn.plugin'
  $copy = -not (Test-Path -LiteralPath $link)
  if (-not $copy -and (Test-Path -LiteralPath $mark)) {
    $copy = (Get-Item -LiteralPath $bin).LastWriteTimeUtc -gt (Get-Item -LiteralPath $link).LastWriteTimeUtc
  }
  if ($copy) {
    New-Item -ItemType Directory -Force -Path $home_ | Out-Null
    Copy-Item -LiteralPath $bin -Destination $link -Force
    New-Item -ItemType File -Force -Path $mark | Out-Null
  }
} catch { }
exit 0
