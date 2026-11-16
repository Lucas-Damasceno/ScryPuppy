$ErrorActionPreference = "Stop"

$repoRoot = Split-Path -Parent $PSScriptRoot
$appRoot = Join-Path $repoRoot "apps\scrypuppy-iced"
$application = Join-Path $appRoot "target\release\scrypuppy-iced.exe"
$installerScript = Join-Path $appRoot "windows\installer.nsi"
$applicationIcon = Join-Path $repoRoot "src-tauri\icons\icon.ico"
$makensis = Join-Path $env:LOCALAPPDATA "tauri\NSIS\makensis.exe"
if (-not (Test-Path -LiteralPath $makensis)) {
  $nsisCommand = Get-Command makensis.exe -ErrorAction SilentlyContinue
  if ($nsisCommand) { $makensis = $nsisCommand.Source }
}
if (-not (Test-Path -LiteralPath $makensis)) {
  throw "NSIS compiler was not found. Install NSIS or the Tauri NSIS toolchain."
}
foreach ($requiredPath in @($application, $installerScript, $applicationIcon)) {
  if (-not (Test-Path -LiteralPath $requiredPath)) {
    throw "Required Iced installer input was not found: $requiredPath"
  }
}

$validationId = [Guid]::NewGuid().ToString("N")
$tempRoot = [System.IO.Path]::GetFullPath([System.IO.Path]::GetTempPath())
$validationRoot = [System.IO.Path]::GetFullPath(
  (Join-Path $tempRoot "scrypuppy-iced-nsis-$validationId")
)
$expectedParent = $tempRoot.TrimEnd([System.IO.Path]::DirectorySeparatorChar) +
  [System.IO.Path]::DirectorySeparatorChar
if (-not $validationRoot.StartsWith($expectedParent, [System.StringComparison]::OrdinalIgnoreCase)) {
  throw "Refusing to use a test directory outside the temporary folder: $validationRoot"
}

$installerPath = Join-Path $validationRoot "ScryPuppy-Iced-Validation.exe"
$installRoot = Join-Path $validationRoot "installed-app"
$installedApplication = Join-Path $installRoot "scrypuppy-iced.exe"
$uninstaller = Join-Path $installRoot "uninstall.exe"
$registryKey = "Software\Microsoft\Windows\CurrentVersion\Uninstall\ScryPuppy-Iced-Validation-$validationId"
$registryPath = "HKCU:\$registryKey"
$installLocationKey = "Software\scryppy\ScryPuppy-Iced-Validation-$validationId"
$installLocationRegistryPath = "HKCU:\$installLocationKey"
$autostartKey = "Software\Microsoft\Windows\CurrentVersion\Run\ScryPuppy-Iced-Validation-$validationId"
$autostartRegistryPath = "HKCU:\$autostartKey"
$autostartValue = "ScryPuppy"
$shortcutName = "ScryPuppy-Iced-Validation-$validationId"
$startMenuShortcut = Join-Path $env:APPDATA "Microsoft\Windows\Start Menu\Programs\$shortcutName.lnk"
$desktopShortcut = Join-Path $env:USERPROFILE "Desktop\$shortcutName.lnk"
$fileVersion = "1.0.0.1"
New-Item -ItemType Directory -Force -Path $validationRoot | Out-Null

try {
  Push-Location $repoRoot
  try {
    & $makensis `
      "/DAPP_VERSION=1.0.0-beta.1" `
      "/DAPP_FILE_VERSION=$fileVersion" `
      "/DAPP_EXE=$application" `
      "/DAPP_ICON=$applicationIcon" `
      "/DOUTPUT_FILE=$installerPath" `
      "/DUNINSTALL_KEY=$registryKey" `
      "/DINSTALL_LOCATION_KEY=$installLocationKey" `
      "/DAUTOSTART_KEY=$autostartKey" `
      "/DAUTOSTART_VALUE=$autostartValue" `
      "/DSHORTCUT_NAME=$shortcutName" `
      $installerScript
    if ($LASTEXITCODE -ne 0) {
      throw "Iced NSIS validation installer compilation failed with exit code $LASTEXITCODE."
    }
  } finally {
    Pop-Location
  }

  if (-not (Test-Path -LiteralPath $installerPath)) {
    throw "Iced NSIS validation installer was not created: $installerPath"
  }

  New-Item -ItemType Directory -Force -Path $autostartRegistryPath | Out-Null
  Set-ItemProperty -LiteralPath $autostartRegistryPath -Name $autostartValue `
    -Value '"C:\Legacy\scrypuppy.exe"'
  New-Item -ItemType Directory -Force -Path $installLocationRegistryPath | Out-Null
  Set-Item -LiteralPath $installLocationRegistryPath -Value $installRoot

  $installerProcess = Start-Process `
    -FilePath $installerPath `
    -ArgumentList "/S" `
    -PassThru `
    -Wait `
    -NoNewWindow
  if ($installerProcess.ExitCode -ne 0) {
    throw "Iced NSIS validation install failed with exit code $($installerProcess.ExitCode)."
  }

  if (-not (Test-Path -LiteralPath $installedApplication)) {
    throw "Iced NSIS validation install did not create the application executable."
  }
  if (-not (Test-Path -LiteralPath $uninstaller)) {
    throw "Iced NSIS validation install did not create its uninstaller."
  }
  if (-not (Test-Path -LiteralPath $startMenuShortcut)) {
    throw "Iced NSIS validation install did not create its Start menu shortcut."
  }
  if (-not (Test-Path -LiteralPath $registryPath)) {
    throw "Iced NSIS validation install did not register the uninstall entry."
  }

  $registration = Get-ItemProperty -LiteralPath $registryPath
  if ($registration.MainBinaryName -ne "scrypuppy-iced.exe") {
    throw "Iced NSIS validation install registered an unexpected main executable."
  }
  if ($registration.InstallLocation.Trim('"') -ne $installRoot) {
    throw "Iced NSIS validation install registered an unexpected install location."
  }
  if ($registration.UninstallString -notmatch [regex]::Escape($uninstaller)) {
    throw "Iced NSIS validation install registered an invalid uninstaller path."
  }
  $autostart = (Get-ItemProperty -LiteralPath $autostartRegistryPath).$autostartValue
  if ($autostart -notmatch [regex]::Escape("$installRoot\scrypuppy-iced.exe")) {
    throw "Iced NSIS validation install did not preserve and update the auto-start entry."
  }

  $uninstallProcess = Start-Process `
    -FilePath $uninstaller `
    -ArgumentList "/S" `
    -PassThru `
    -Wait `
    -NoNewWindow
  if ($uninstallProcess.ExitCode -ne 0) {
    throw "Iced NSIS validation uninstall failed with exit code $($uninstallProcess.ExitCode)."
  }
  if (Test-Path -LiteralPath $installRoot) {
    throw "Iced NSIS validation uninstall left the test installation directory behind."
  }
  if (Test-Path -LiteralPath $registryPath) {
    throw "Iced NSIS validation uninstall left its test registry entry behind."
  }
  if ((Get-ItemProperty -LiteralPath $autostartRegistryPath -ErrorAction SilentlyContinue).$autostartValue) {
    throw "Iced NSIS validation uninstall left its auto-start value behind."
  }
  if (Test-Path -LiteralPath $installLocationRegistryPath) {
    throw "Iced NSIS validation uninstall left its install-location key behind."
  }
  if ((Test-Path -LiteralPath $startMenuShortcut) -or (Test-Path -LiteralPath $desktopShortcut)) {
    throw "Iced NSIS validation uninstall left a test shortcut behind."
  }

  Write-Host "Iced NSIS install and uninstall smoke test passed."
} finally {
  try {
    if (Test-Path -LiteralPath $uninstaller) {
      $cleanupProcess = Start-Process `
        -FilePath $uninstaller `
        -ArgumentList "/S" `
        -PassThru `
        -Wait `
        -NoNewWindow
      if ($cleanupProcess.ExitCode -ne 0) {
        Write-Warning "Validation uninstaller returned $($cleanupProcess.ExitCode) during cleanup."
      }
    }

    if (Test-Path -LiteralPath $registryPath) {
      Remove-Item -LiteralPath $registryPath -Force
    }
    foreach ($registryTestPath in @($autostartRegistryPath, $installLocationRegistryPath)) {
      if (Test-Path -LiteralPath $registryTestPath) {
        Remove-Item -LiteralPath $registryTestPath -Force
      }
    }
    foreach ($path in @(
      $installedApplication,
      (Join-Path $installRoot "scrypuppy.ico"),
      $startMenuShortcut,
      $desktopShortcut,
      $uninstaller,
      $installerPath
    )) {
      if (Test-Path -LiteralPath $path) {
        Remove-Item -LiteralPath $path -Force
      }
    }
    if (Test-Path -LiteralPath $installRoot) {
      Remove-Item -LiteralPath $installRoot -Force
    }
    if (Test-Path -LiteralPath $validationRoot) {
      $resolvedRoot = [System.IO.Path]::GetFullPath($validationRoot)
      if (-not $resolvedRoot.StartsWith($expectedParent, [System.StringComparison]::OrdinalIgnoreCase)) {
        throw "Refusing to remove a test directory outside the temporary folder: $resolvedRoot"
      }
      Remove-Item -LiteralPath $resolvedRoot -Force
    }
  } catch {
    Write-Warning "Iced NSIS temporary install cleanup was incomplete: $($_.Exception.Message)"
  }
}
