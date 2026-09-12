param(
    [Parameter(Mandatory=$true)][ValidateSet('build','verify')][string]$Mode,
    [Parameter(Mandatory=$true)][string]$Target
)
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root
$vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio/Installer/vswhere.exe'
$installation = & $vswhere -latest -version '[17.0,18.0)' -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
if (-not $installation) { throw 'Visual Studio 2022 native C++ toolchain is required' }
$arch = if ($Target.StartsWith('aarch64')) { 'arm64' } else { 'amd64' }
$devcmd = Join-Path $installation 'Common7/Tools/VsDevCmd.bat'
$environment = & cmd.exe /d /c "call `"$devcmd`" -no_logo -arch=$arch -host_arch=$arch && set"
foreach ($line in $environment) {
    if ($line -match '^([^=]+)=(.*)$') { [Environment]::SetEnvironmentVariable($Matches[1], $Matches[2], 'Process') }
}
$env:PATH = "C:\Program Files\LLVM\bin;$env:PATH"
$toolchain = (Get-Content rust-toolchain -Raw).Trim()
$env:RUSTUP_TOOLCHAIN = "$toolchain-$Target"
rustup toolchain install $env:RUSTUP_TOOLCHAIN --profile minimal --no-self-update
if ($Mode -eq 'build') {
    if ($Target.StartsWith('aarch64')) {
        # Visual Studio 生成器不生成 BoringSSL GNU 风格 ARM 汇编的构建步骤。
        $env:CMAKE_GENERATOR = 'Ninja'
        # AWS-LC 自带 ARM 汇编的 VS 集成，不继承 BoringSSL 的生成器选择。
        $env:AWS_LC_SYS_CMAKE_GENERATOR = 'Visual Studio 17 2022'
        $env:CC = 'clang-cl'
        $env:CXX = 'clang-cl'
    }
    python scripts/sdk-package.py --target $Target
} else {
    git fetch bound/sdk-bound.bundle refs/heads/sdk-bound:refs/heads/sdk-bound
    git checkout sdk-bound
    $revision = (git rev-parse HEAD).Trim()
    $temp = Join-Path $env:RUNNER_TEMP 'sdk-verification'
    New-Item -ItemType Directory -Path $temp -Force | Out-Null

    $cache = Join-Path $temp 'cache'
    $consumer = Join-Path $temp 'consumer'
    python scripts/sdk-bind.py seed --target $Target --artifacts artifacts --cache $cache
    $repository = ([uri]($root.Replace('\','/') + '/')).AbsoluteUri
    New-Item -ItemType Directory -Path dist/evidence -Force | Out-Null
    try {
        rustup toolchain install "1.98.1-$Target" --profile minimal --no-self-update 2>&1 | Tee-Object -FilePath dist/evidence/cross-rust-1.98.1-install.log
        if ($LASTEXITCODE -ne 0) { throw "Required native Rust 1.98.1 toolchain unavailable: $Target" }
        python sdk-consumer/verify.py --sdk-revision $revision --repository $repository --destination $consumer --target $Target --cache-dir $cache
        python scripts/sdk-audit.py --target $Target --consumer $consumer --output dist/evidence/runtime.json
        python scripts/sdk-audit.py --target $Target --consumer (Join-Path $consumer 'cross-rust-1.98.1') --profile debug --output dist/evidence/runtime-rust-1.98.1.json
    } finally {
        if (Test-Path $consumer) {
            Get-ChildItem $consumer -File | Where-Object { $_.Extension -in '.log','.json','.jsonl' } | Copy-Item -Destination dist/evidence
            if (Test-Path (Join-Path $consumer 'evidence')) {
                Copy-Item (Join-Path $consumer 'evidence') dist/evidence/consumer -Recurse -Force
            }
        }
    }
}
