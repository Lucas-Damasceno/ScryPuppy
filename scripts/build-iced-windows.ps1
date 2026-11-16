$ErrorActionPreference = "Stop"

$repoRoot = Split-Path -Parent $PSScriptRoot
$appRoot = Join-Path $repoRoot "apps\scrypuppy-iced"
$manifest = Join-Path $appRoot "Cargo.toml"
$releaseRoot = Join-Path $appRoot "target\release"
$application = Join-Path $releaseRoot "scrypuppy-iced.exe"
$installerScript = Join-Path $appRoot "windows\installer.nsi"
$applicationIcon = Join-Path $repoRoot "src-tauri\icons\icon.ico"
$metadata = (& (Join-Path $PSScriptRoot "release-metadata.ps1")) | ConvertFrom-Json
$bundleRoot = Split-Path -Parent $metadata.installer_path
$installerName = "ScryPuppy_Iced_beta_installer_v$($metadata.public_version).exe"
$installerPath = Join-Path $bundleRoot $installerName
$checksumPath = "$installerPath.sha256"
$signaturePath = "$installerPath.sig"
$makensis = Join-Path $env:LOCALAPPDATA "tauri\NSIS\makensis.exe"

if (-not (Test-Path -LiteralPath $makensis)) {
  $nsisCommand = Get-Command makensis.exe -ErrorAction SilentlyContinue
  if ($nsisCommand) { $makensis = $nsisCommand.Source }
}
if (-not (Test-Path -LiteralPath $makensis)) {
  throw "NSIS compiler was not found. Install NSIS or the Tauri NSIS toolchain."
}

$hasSigningKey = -not [string]::IsNullOrWhiteSpace($env:TAURI_SIGNING_PRIVATE_KEY)
$signatureRequired = $env:REQUIRE_UPDATER_SIGNATURE -eq "true"
if ($signatureRequired -and -not $hasSigningKey) {
  throw "Updater signing is required, but TAURI_SIGNING_PRIVATE_KEY is not configured."
}
if (-not (Test-Path -LiteralPath $application)) {
  throw "Iced executable was not found: $application. Build it with Cargo first."
}
if (-not (Test-Path -LiteralPath $applicationIcon)) {
  throw "ScryPuppy application icon was not found: $applicationIcon"
}
if (-not (Test-Path -LiteralPath $installerScript)) {
  throw "Iced NSIS definition was not found: $installerScript"
}

New-Item -ItemType Directory -Force -Path $bundleRoot | Out-Null
$fileVersion = $metadata.manifest_version -replace '-beta\.', '.'
Push-Location $repoRoot
try {
  & $makensis `
    "/DAPP_VERSION=$($metadata.manifest_version)" `
    "/DAPP_FILE_VERSION=$fileVersion" `
    "/DAPP_EXE=$application" `
    "/DAPP_ICON=$applicationIcon" `
    "/DOUTPUT_FILE=$installerPath" `
    $installerScript
  if ($LASTEXITCODE -ne 0) {
    throw "Iced NSIS packaging failed with exit code $LASTEXITCODE."
  }

  if (-not (Test-Path -LiteralPath $installerPath)) {
    throw "Expected Iced NSIS installer was not created: $installerPath"
  }
  if (Test-Path -LiteralPath $signaturePath) {
    Remove-Item -LiteralPath $signaturePath -Force
  }
  if ($hasSigningKey) {
    & npm run tauri -- signer sign $installerPath
    if ($LASTEXITCODE -ne 0) {
      throw "Iced updater signing failed with exit code $LASTEXITCODE."
    }
    if (-not (Test-Path -LiteralPath $signaturePath)) {
      throw "Expected Iced updater signature was not created: $signaturePath"
    }
  }

  $hash = & (Join-Path $PSScriptRoot "file-sha256.ps1") -Path $installerPath
  "$hash  $installerName" | Set-Content -LiteralPath $checksumPath -Encoding ascii
} finally {
  Pop-Location
}

[pscustomobject]@{
  installer_name = $installerName
  installer_path = $installerPath
  checksum_path = $checksumPath
  signature_path = $signaturePath
  manifest_version = $metadata.manifest_version
} | ConvertTo-Json -Compress
