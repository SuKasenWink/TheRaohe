param(
    [string]$Api = "34",
    [string]$Ndk = "",
    [switch]$Release
)

$ErrorActionPreference = "Stop"

function Resolve-Ndk {
    param([string]$Explicit)
    if ($Explicit) {
        if (Test-Path -LiteralPath $Explicit) { return (Resolve-Path -LiteralPath $Explicit).Path }
        throw "NDK path not found: $Explicit"
    }
    foreach ($v in @($env:ANDROID_NDK_HOME, $env:ANDROID_NDK_ROOT)) {
        if ($v -and (Test-Path -LiteralPath $v)) { return (Resolve-Path -LiteralPath $v).Path }
    }
    $roots = @()
    if ($env:ANDROID_SDK_ROOT) { $roots += $env:ANDROID_SDK_ROOT }
    if ($env:ANDROID_HOME) { $roots += $env:ANDROID_HOME }
    if ($env:LOCALAPPDATA) { $roots += (Join-Path $env:LOCALAPPDATA "Android\Sdk") }
    $roots += "C:\Android\Sdk"
    foreach ($r in $roots) {
        $ndkDir = Join-Path $r "ndk"
        if (Test-Path -LiteralPath $ndkDir) {
            $cand = Get-ChildItem -LiteralPath $ndkDir -Directory |
                Sort-Object { [version]($_.Name -replace '[^0-9.]', '') } -Descending |
                Select-Object -First 1
            if ($cand) { return $cand.FullName }
        }
    }
    throw "No NDK found. Set ANDROID_NDK_HOME or pass -Ndk <path>. Install with: sdkmanager 'ndk;27.0.12077973'"
}

$ndkPath = Resolve-Ndk $Ndk
$clang = Join-Path $ndkPath "toolchains\llvm\prebuilt\windows-x86_64\bin\clang.exe"
if (-not (Test-Path -LiteralPath $clang)) {
    throw "clang not found: $clang"
}

$env:CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER = $clang
$env:CARGO_TARGET_AARCH64_LINUX_ANDROID_RUSTFLAGS = "-Clink-arg=--target=aarch64-linux-android$Api -Clink-arg=-fuse-ld=lld -Clink-arg=-Wl,-z,max-page-size=16384"
$env:CC_aarch64_linux_android = $clang
$env:AR_aarch64_linux_android = Join-Path $ndkPath "toolchains\llvm\prebuilt\windows-x86_64\bin\llvm-ar.exe"

Write-Output "NDK:   $ndkPath"
Write-Output "clang: $clang"
Write-Output "API:   $Api"

$cargoArgs = @("build", "--target", "aarch64-linux-android")
if ($Release) { $cargoArgs += "--release" }
& cargo @cargoArgs
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

$profile = if ($Release) { "release" } else { "debug" }
foreach ($name in @("theraohe")) {
    $out = Join-Path $PSScriptRoot "target\aarch64-linux-android\$profile\$name"
    if (Test-Path -LiteralPath $out) {
        Write-Output "built: $out"
        Write-Output ("size:  {0} bytes" -f (Get-Item -LiteralPath $out).Length)
    }
}
