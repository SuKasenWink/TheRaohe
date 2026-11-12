param(
    [string]$Serial = "",
    [string]$RemoteDir = "/data/local/tmp",
    [string]$RunArgs = "--show",
    [switch]$SkipBuild,
    [switch]$Restart,
    [switch]$Permissive,
    [switch]$Background
)

$ErrorActionPreference = "Stop"

function Resolve-Adb {
    $cmd = Get-Command adb -ErrorAction SilentlyContinue
    if ($cmd) { return $cmd.Source }
    $roots = @()
    if ($env:ANDROID_SDK_ROOT) { $roots += $env:ANDROID_SDK_ROOT }
    if ($env:ANDROID_HOME) { $roots += $env:ANDROID_HOME }
    if ($env:LOCALAPPDATA) { $roots += (Join-Path $env:LOCALAPPDATA "Android\Sdk") }
    $roots += "C:\Android\Sdk"
    foreach ($r in $roots) {
        $p = Join-Path $r "platform-tools\adb.exe"
        if (Test-Path -LiteralPath $p) { return $p }
    }
    throw "adb not found. Install platform-tools or add it to PATH."
}

$adb = Resolve-Adb

function Invoke-Adb {
    param([string[]]$AdbArgs, [switch]$IgnoreExitCode)
    if ($Serial) {
        & $adb -s $Serial @AdbArgs
    } else {
        & $adb @AdbArgs
    }
    if ($LASTEXITCODE -ne 0 -and -not $IgnoreExitCode) { throw "adb $($AdbArgs -join ' ') failed (exit $LASTEXITCODE)" }
}

function Assert-Root {
    $out = if ($Serial) { & $adb -s $Serial shell su -c id } else { & $adb shell su -c id }
    if ($LASTEXITCODE -ne 0 -or -not ($out -match "uid=0")) {
        throw "su did not grant root. Grant root to Shell in KernelSU/Magisk, then retry."
    }
}

$exe = Join-Path $PSScriptRoot "target\aarch64-linux-android\release\theraohe"
if (-not $SkipBuild) {
    & (Join-Path $PSScriptRoot "build.ps1") -Release
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
}
if (-not (Test-Path -LiteralPath $exe)) {
    throw "binary not found: $exe"
}

$devices = & $adb devices | Select-Object -Skip 1
$ready = @($devices | Where-Object { $_ -match "\tdevice$" })
if ($ready.Count -eq 0) { throw "no authorized device found (adb devices)" }
if ($ready.Count -gt 1 -and -not $Serial) {
    throw "multiple devices found; rerun with -Serial <serial>"
}

Assert-Root

$remote = "$RemoteDir/theraohe"

if ($Restart) {
    # 不用 -x: 連帶殺掉 theraohe-dbg 這類子字串比對得到的除錯實例
    Invoke-Adb @("shell", "su", "-c", "pkill theraohe") -IgnoreExitCode
}

Invoke-Adb @("push", $exe, $remote)
Invoke-Adb @("shell", "chmod", "755", $remote)

if ($Permissive) {
    Invoke-Adb @("shell", "su", "-c", "setenforce 0")
    Write-Output "SELinux: permissive"
}

$cmd = $remote
if ($RunArgs) { $cmd = "$remote $RunArgs" }

Write-Output "run: $cmd"

if ($Background) {
    $full = @("shell", "su", "-c", $cmd)
    if ($Serial) { $full = @("-s", $Serial) + $full }
    Start-Process -FilePath $adb -ArgumentList $full -NoNewWindow
} else {
    if ($Serial) {
        & $adb -s $Serial shell su -c $cmd
    } else {
        & $adb shell su -c $cmd
    }
    exit $LASTEXITCODE
}
