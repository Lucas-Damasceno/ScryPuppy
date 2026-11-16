param(
  [Parameter(Mandatory = $true)]
  [string]$TauriInstaller,
  [Parameter(Mandatory = $true)]
  [string]$TauriSignature,
  [Parameter(Mandatory = $true)]
  [string]$IcedInstaller
)

$ErrorActionPreference = "Stop"

$TauriInstaller = [System.IO.Path]::GetFullPath($TauriInstaller)
$TauriSignature = [System.IO.Path]::GetFullPath($TauriSignature)
$IcedInstaller = [System.IO.Path]::GetFullPath($IcedInstaller)
foreach ($installerPath in @($TauriInstaller, $TauriSignature, $IcedInstaller)) {
  if (-not (Test-Path -LiteralPath $installerPath -PathType Leaf)) {
    throw "Required migration installer was not found: $installerPath"
  }
}

$uninstallKey = "Software\Microsoft\Windows\CurrentVersion\Uninstall\ScryPuppy"
$uninstallRegistryPath = "HKCU:\$uninstallKey"
$installLocationKey = "Software\scryppy\ScryPuppy"
$installLocationRegistryPath = "HKCU:\$installLocationKey"
$autostartKey = "Software\Microsoft\Windows\CurrentVersion\Run"
$autostartRegistryPath = "HKCU:\$autostartKey"
$autostartValue = "ScryPuppy"
if (Test-Path -LiteralPath $uninstallRegistryPath) {
  throw "Refusing to run migration validation over an existing ScryPuppy installation."
}
if (Test-Path -LiteralPath $installLocationRegistryPath) {
  throw "Refusing to replace the existing ScryPuppy install-location registry key."
}
if ((Get-ItemProperty -LiteralPath $autostartRegistryPath -ErrorAction SilentlyContinue).$autostartValue) {
  throw "Refusing to replace the existing ScryPuppy auto-start entry."
}

$validationId = [Guid]::NewGuid().ToString("N")
$tempRoot = [System.IO.Path]::GetFullPath([System.IO.Path]::GetTempPath())
$validationRoot = [System.IO.Path]::GetFullPath(
  (Join-Path $tempRoot "scrypuppy-tauri-iced-upgrade-$validationId")
)
$expectedParent = $tempRoot.TrimEnd([System.IO.Path]::DirectorySeparatorChar) +
  [System.IO.Path]::DirectorySeparatorChar
if (-not $validationRoot.StartsWith($expectedParent, [System.StringComparison]::OrdinalIgnoreCase)) {
  throw "Refusing to use a validation directory outside the temporary folder: $validationRoot"
}

$appManifest = Join-Path $PSScriptRoot "..\apps\scrypuppy-iced\Cargo.toml"
$oldFixtureBinary = $env:SCRYPUPPY_UPDATE_TEST_BINARY
$oldFixtureSignature = $env:SCRYPUPPY_UPDATE_TEST_SIGNATURE
try {
  $env:SCRYPUPPY_UPDATE_TEST_BINARY = $TauriInstaller
  $env:SCRYPUPPY_UPDATE_TEST_SIGNATURE = $TauriSignature
  Push-Location (Split-Path -Parent $PSScriptRoot)
  try {
    cargo test --manifest-path $appManifest --locked `
      updater::tests::verifies_downloaded_installer_signature_fixture_when_configured -- --exact
    if ($LASTEXITCODE -ne 0) {
      throw "The released Tauri installer signature did not verify with the Iced update key."
    }
  } finally {
    Pop-Location
  }
} finally {
  $env:SCRYPUPPY_UPDATE_TEST_BINARY = $oldFixtureBinary
  $env:SCRYPUPPY_UPDATE_TEST_SIGNATURE = $oldFixtureSignature
}

$installRoot = Join-Path $validationRoot "installed-app"
$dataRoot = Join-Path $env:APPDATA "com.scryppy.desktop"
$dataMarker = Join-Path $dataRoot ".iced-migration-validation-$validationId"
$startMenuShortcut = Join-Path $env:APPDATA "Microsoft\Windows\Start Menu\Programs\ScryPuppy.lnk"
$desktopShortcut = Join-Path $env:USERPROFILE "Desktop\ScryPuppy.lnk"
if ((Test-Path -LiteralPath $startMenuShortcut) -or (Test-Path -LiteralPath $desktopShortcut)) {
  throw "Refusing to replace an existing ScryPuppy shortcut during migration validation."
}
$createdDataRoot = -not (Test-Path -LiteralPath $dataRoot)
$tauriRegistration = $null
$icedUninstaller = Join-Path $installRoot "uninstall.exe"
New-Item -ItemType Directory -Force -Path $validationRoot | Out-Null

try {
  New-Item -ItemType Directory -Force -Path $dataRoot | Out-Null
  "ScryPuppy user data must survive an application upgrade and uninstall." |
    Set-Content -LiteralPath $dataMarker -Encoding utf8

  New-Item -ItemType Directory -Force -Path $installLocationRegistryPath | Out-Null
  Set-Item -LiteralPath $installLocationRegistryPath -Value $installRoot
  New-Item -ItemType Directory -Force -Path $autostartRegistryPath | Out-Null
  Set-ItemProperty -LiteralPath $autostartRegistryPath -Name $autostartValue -Value '"C:\Legacy\scrypuppy.exe"'

  $tauriInstall = Start-Process `
    -FilePath $TauriInstaller `
    -ArgumentList @("/S", "/D=$installRoot") `
    -PassThru `
    -Wait `
    -NoNewWindow
  if ($tauriInstall.ExitCode -ne 0) {
    throw "Tauri baseline install failed with exit code $($tauriInstall.ExitCode)."
  }

  if (-not (Test-Path -LiteralPath $uninstallRegistryPath)) {
    throw "Tauri baseline installer did not create its uninstall registration."
  }
  $tauriRegistration = Get-ItemProperty -LiteralPath $uninstallRegistryPath
  $tauriBinaryName = [string]$tauriRegistration.MainBinaryName
  if ([string]::IsNullOrWhiteSpace($tauriBinaryName)) {
    throw "Tauri baseline uninstall registration did not identify its installed executable."
  }
  $tauriExecutableFileName = $tauriBinaryName
  if ([System.IO.Path]::GetExtension($tauriExecutableFileName) -ne ".exe") {
    $tauriExecutableFileName += ".exe"
  }
  $tauriBinary = Join-Path $installRoot $tauriExecutableFileName
  if (-not (Test-Path -LiteralPath $tauriBinary)) {
    throw "Tauri baseline executable was not installed at the isolated path: $tauriBinary"
  }
  if ((Get-ItemProperty -LiteralPath $autostartRegistryPath).$autostartValue -notmatch [regex]::Escape($tauriBinary)) {
    throw "Tauri baseline installer did not preserve its seeded auto-start setting."
  }

  $icedInstall = Start-Process `
    -FilePath $IcedInstaller `
    -ArgumentList "/S" `
    -PassThru `
    -Wait `
    -NoNewWindow
  if ($icedInstall.ExitCode -ne 0) {
    throw "Iced migration install failed with exit code $($icedInstall.ExitCode)."
  }

  $icedBinary = Join-Path $installRoot "scrypuppy-iced.exe"
  if (-not (Test-Path -LiteralPath $icedBinary)) {
    throw "Iced migration did not install its executable into the existing Tauri install directory."
  }
  if (Test-Path -LiteralPath $tauriBinary) {
    throw "Iced migration left the prior Tauri executable behind: $tauriBinary"
  }
  if (-not (Test-Path -LiteralPath $dataMarker)) {
    throw "Iced migration modified the user's application data directory."
  }
  $icedRegistration = Get-ItemProperty -LiteralPath $uninstallRegistryPath
  if ($icedRegistration.MainBinaryName -ne "scrypuppy-iced.exe") {
    throw "Iced migration did not replace the uninstall registration's main executable."
  }
  if ($icedRegistration.InstallLocation.Trim('"') -ne $installRoot) {
    throw "Iced migration changed the existing installation location."
  }
  if ((Get-ItemProperty -LiteralPath $autostartRegistryPath).$autostartValue -notmatch [regex]::Escape($icedBinary)) {
    throw "Iced migration did not preserve and update the existing auto-start entry."
  }

  $uninstall = Start-Process `
    -FilePath $icedUninstaller `
    -ArgumentList "/S" `
    -PassThru `
    -Wait `
    -NoNewWindow
  if ($uninstall.ExitCode -ne 0) {
    throw "Iced migration uninstall failed with exit code $($uninstall.ExitCode)."
  }
  if (-not (Test-Path -LiteralPath $dataMarker)) {
    throw "Iced uninstall removed user data during an application-only uninstall."
  }

  Write-Host "Tauri-to-Iced upgrade preserved the install location, startup setting, and user data."
} finally {
  try {
    if (Test-Path -LiteralPath $icedUninstaller) {
      $cleanup = Start-Process `
        -FilePath $icedUninstaller `
        -ArgumentList "/S" `
        -PassThru `
        -Wait `
        -NoNewWindow
      if ($cleanup.ExitCode -ne 0) {
        Write-Warning "Iced migration uninstaller returned $($cleanup.ExitCode) during cleanup."
      }
    }
    if (Test-Path -LiteralPath $uninstallRegistryPath) {
      Remove-Item -LiteralPath $uninstallRegistryPath -Force
    }
    if (Test-Path -LiteralPath $installLocationRegistryPath) {
      Remove-Item -LiteralPath $installLocationRegistryPath -Force
    }
    if (Test-Path -LiteralPath $autostartRegistryPath) {
      Remove-ItemProperty -LiteralPath $autostartRegistryPath -Name $autostartValue -ErrorAction SilentlyContinue
    }
    if (Test-Path -LiteralPath $dataMarker) {
      Remove-Item -LiteralPath $dataMarker -Force
    }
    foreach ($shortcutPath in @($startMenuShortcut, $desktopShortcut)) {
      if (Test-Path -LiteralPath $shortcutPath) {
        Remove-Item -LiteralPath $shortcutPath -Force
      }
    }
    if ($createdDataRoot -and (Test-Path -LiteralPath $dataRoot) -and
      -not (Get-ChildItem -LiteralPath $dataRoot -Force | Select-Object -First 1)) {
      Remove-Item -LiteralPath $dataRoot -Force
    }
    if (Test-Path -LiteralPath $validationRoot) {
      $resolvedRoot = [System.IO.Path]::GetFullPath($validationRoot)
      if (-not $resolvedRoot.StartsWith($expectedParent, [System.StringComparison]::OrdinalIgnoreCase)) {
        throw "Refusing to remove a validation directory outside the temporary folder: $resolvedRoot"
      }
      Remove-Item -LiteralPath $resolvedRoot -Recurse -Force
    }
  } catch {
    Write-Warning "Tauri-to-Iced validation cleanup was incomplete: $($_.Exception.Message)"
  }
}
