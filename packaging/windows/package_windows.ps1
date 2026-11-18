param(
    [string]$Version = "",
    [string]$OutDir = ""
)

$ErrorActionPreference = "Stop"

$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$RepoRoot = (Resolve-Path (Join-Path $ScriptDir "..\..")).Path

if ([string]::IsNullOrWhiteSpace($OutDir)) {
    $OutDir = Join-Path $RepoRoot "dist"
}

Push-Location $RepoRoot
try {
    if ([string]::IsNullOrWhiteSpace($Version)) {
        $CargoToml = Get-Content -Path "Cargo.toml"
        $InWorkspacePkg = $false
        foreach ($Line in $CargoToml) {
            if ($Line -match '^\[workspace\.package\]') {
                $InWorkspacePkg = $true
                continue
            }
            if ($Line -match '^\[') {
                $InWorkspacePkg = $false
            }
            if ($InWorkspacePkg -and $Line -match '^version\s*=\s*"([^"]+)"') {
                $Version = $Matches[1]
                break
            }
        }
    }

    $Version = $Version.TrimStart('v')
    if ([string]::IsNullOrWhiteSpace($Version)) {
        throw "Failed to determine package version."
    }

    $env:LIBOPUS_STATIC = "1"
    $env:LIBOPUS_NO_PKG = "1"
    $env:CMAKE_POLICY_VERSION_MINIMUM = "3.5"

    Write-Host "==> Building rsrelay-gui.exe (v$Version) for x86_64-pc-windows-msvc..."
    cargo build --release -p rsrelay-gui --target x86_64-pc-windows-msvc
    if ($LASTEXITCODE -ne 0) {
        throw "cargo build failed with exit code $LASTEXITCODE"
    }

    $ExePath = Join-Path $RepoRoot "target\x86_64-pc-windows-msvc\release\rsrelay-gui.exe"
    if (-not (Test-Path $ExePath)) {
        throw "Built binary not found at $ExePath"
    }

    Write-Host "==> Running headless binary smoke test (--check)..."
    & $ExePath --check
    if ($LASTEXITCODE -ne 0) {
        throw "rsrelay-gui.exe --check failed with exit code $LASTEXITCODE"
    }

    New-Item -ItemType Directory -Force -Path $OutDir | Out-Null

    # 1. Create portable ZIP package
    $PortableName = "rsrelay-v$Version-windows-x64-portable.zip"
    $PortableZip = Join-Path $OutDir $PortableName
    if (Test-Path $PortableZip) {
        Remove-Item -Force $PortableZip
    }
    Write-Host "==> Creating portable archive: $PortableZip"
    Compress-Archive -Path $ExePath -DestinationPath $PortableZip -CompressionLevel Optimal

    # 2. Fetch Inno Setup Simplified Chinese translation if not already present (best-effort)
    $ZhIslPath = Join-Path $ScriptDir "ChineseSimplified.isl"
    if (-not (Test-Path $ZhIslPath)) {
        try {
            Write-Host "==> Downloading Inno Setup ChineseSimplified.isl..."
            $Resp = Invoke-WebRequest `
                -Uri "https://raw.githubusercontent.com/jrsoftware/issrc/main/Files/Languages/Unofficial/ChineseSimplified.isl" `
                -UseBasicParsing `
                -TimeoutSec 15
            if ($Resp.Content -match '\[LangOptions\]') {
                $Utf8Bom = New-Object System.Text.UTF8Encoding($true)
                [System.IO.File]::WriteAllText($ZhIslPath, $Resp.Content, $Utf8Bom)
            } else {
                Write-Warning "Unexpected content for ChineseSimplified.isl; skipping."
            }
        } catch {
            Write-Warning "Could not download ChineseSimplified.isl; falling back to English installer UI: $_"
        }
    }

    # 3. Locate Inno Setup Compiler (ISCC.exe)
    $IsccCmd = Get-Command "iscc.exe" -ErrorAction SilentlyContinue
    if ($null -ne $IsccCmd) {
        $IsccPath = $IsccCmd.Source
    } else {
        $CandidatePaths = @(
            "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe",
            "${env:ProgramFiles}\Inno Setup 6\ISCC.exe",
            "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe"
        )
        $IsccPath = $CandidatePaths | Where-Object { Test-Path $_ } | Select-Object -First 1
    }

    if ([string]::IsNullOrWhiteSpace($IsccPath)) {
        throw "Inno Setup 6 (ISCC.exe) not found. Install via: winget install JRSoftware.InnoSetup"
    }

    $IssFile = Join-Path $ScriptDir "rsrelay.iss"
    Write-Host "==> Building Windows Setup installer with ISCC: $IsccPath"
    & $IsccPath `
        "/DMyAppVersion=$Version" `
        "/DBinPath=$ExePath" `
        "/DOutDir=$OutDir" `
        $IssFile

    if ($LASTEXITCODE -ne 0) {
        throw "ISCC.exe failed with exit code $LASTEXITCODE"
    }

    Write-Host "==> Windows artifacts ready in $OutDir"
    Get-ChildItem -Path $OutDir
} finally {
    Pop-Location
}
