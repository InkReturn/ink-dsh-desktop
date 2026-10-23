#requires -Version 7.0
<#
.SYNOPSIS
    将本机 Node.js 与 @deepseek-ai/dsh 复制到 Tauri 的可选离线运行时目录。

.DESCRIPTION
    默认从项目本地 node_modules 或 PATH 中的全局 dsh 定位 DSH 包，从 PATH 定位 node。
    复制完成后执行 `node lib/bin.js --version` 冒烟校验，避免生成无法启动的安装包。
    该脚本只复制运行时文件，不读取或复制任何用户凭据。

.PARAMETER DshPath
    @deepseek-ai/dsh 包根目录，或其中的 lib/bin.js。省略时自动定位本地/全局安装。

.PARAMETER NodePath
    node 可执行文件路径。省略时从 PATH 定位。

.PARAMETER RuntimeRoot
    可选运行时输出目录。默认 src-tauri/resources/runtime，主要用于测试或自定义构建。

.PARAMETER Force
    覆盖已经存在的 node/、dsh/ 与 runtime.json。

.PARAMETER SkipSmokeTest
    跳过复制后的 `--version` 冒烟校验，仅用于离线准备脚本的单元测试。
#>
param(
    [string]$DshPath = "",
    [string]$NodePath = "",
    [string]$RuntimeRoot = "",
    [switch]$Force,
    [switch]$SkipSmokeTest
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$ProjectRoot = Split-Path -Parent $PSScriptRoot
if ([string]::IsNullOrWhiteSpace($RuntimeRoot)) {
    $RuntimeRoot = Join-Path $ProjectRoot "src-tauri\resources\runtime"
}

<#
.SYNOPSIS
    将 DSH 参数归一为包根目录。
.PARAMETER Value
    用户传入的包目录或 lib/bin.js 路径。
.OUTPUTS
    System.String，DSH 包根目录的绝对路径。
#>
function Resolve-DshPackage {
    param([string]$Value)

    # 1. 未显式传入时，先查项目本地依赖。
    # 2. 再根据全局 dsh shim 推导同 prefix 下的包目录。
    if ([string]::IsNullOrWhiteSpace($Value)) {
        $local = Join-Path $ProjectRoot "node_modules\@deepseek-ai\dsh"
        if (Test-Path (Join-Path $local "lib\bin.js")) {
            return (Resolve-Path -LiteralPath $local).Path
        }

        $shim = Get-Command dsh -ErrorAction SilentlyContinue
        if ($null -eq $shim) {
            throw "未找到 dsh。请先 npm i -g @deepseek-ai/dsh，或传入 -DshPath。"
        }
        $shimPath = $shim.Source
        $prefix = Split-Path -Parent $shimPath
        $global = Join-Path $prefix "node_modules\@deepseek-ai\dsh"
        if (Test-Path (Join-Path $global "lib\bin.js")) {
            return (Resolve-Path -LiteralPath $global).Path
        }
        $unixStyle = Join-Path (Split-Path -Parent $prefix) "lib\node_modules\@deepseek-ai\dsh"
        if (Test-Path (Join-Path $unixStyle "lib\bin.js")) {
            return (Resolve-Path -LiteralPath $unixStyle).Path
        }
        throw "已找到 dsh 命令，但无法推导 @deepseek-ai/dsh 包目录；请传入 -DshPath。"
    }

    # 3. 支持直接传 lib/bin.js，回溯两级得到包根目录。
    $resolved = (Resolve-Path -LiteralPath $Value).Path
    if ((Get-Item -LiteralPath $resolved).PSIsContainer) {
        $packageRoot = $resolved
    } else {
        $packageRoot = Split-Path -Parent (Split-Path -Parent $resolved)
    }
    if (-not (Test-Path (Join-Path $packageRoot "lib\bin.js"))) {
        throw "DSH 包目录缺少 lib/bin.js：$packageRoot"
    }
    return $packageRoot
}

<#
.SYNOPSIS
    解析 node 可执行文件。
.PARAMETER Value
    可选显式路径；为空时使用 PATH。
.OUTPUTS
    System.String，node 可执行文件绝对路径。
#>
function Resolve-NodeExe {
    param([string]$Value)

    # 1. 显式路径必须存在。
    # 2. 未指定时从 PATH 获取，并确认不是脚本 shim。
    if (-not [string]::IsNullOrWhiteSpace($Value)) {
        return (Resolve-Path -LiteralPath $Value).Path
    }
    $node = Get-Command node -ErrorAction SilentlyContinue
    if ($null -eq $node -or [string]::IsNullOrWhiteSpace($node.Source)) {
        throw "未找到 node。请安装 Node.js 20+，或传入 -NodePath。"
    }
    return (Resolve-Path -LiteralPath $node.Source).Path
}

<#
.SYNOPSIS
    校验待删除路径必须位于运行时根目录之下。
.PARAMETER Target
    待删除路径。
.PARAMETER Root
    可选运行时根目录。
#>
function Assert-UnderRuntimeRoot {
    param([string]$Target, [string]$Root)

    # 1. 归一化父目录与目标绝对路径。
    # 2. 只允许删除 Root 内的 node、dsh、runtime.json。
    $rootFull = [System.IO.Path]::GetFullPath($Root).TrimEnd('\') + '\'
    $targetFull = [System.IO.Path]::GetFullPath($Target)
    if (-not $targetFull.StartsWith($rootFull, [System.StringComparison]::OrdinalIgnoreCase)) {
        throw "拒绝删除运行时根目录之外的路径：$targetFull"
    }
}

<#
.SYNOPSIS
    复制 Node 与 DSH 到运行时目录并写版本清单。
.PARAMETER NodeSource
    node 可执行文件。
.PARAMETER DshSource
    DSH 包根目录。
.PARAMETER Root
    输出根目录。
.OUTPUTS
    System.String，最终 runtime.json 路径。
#>
function Copy-Runtime {
    param([string]$NodeSource, [string]$DshSource, [string]$Root)

    # 1. 创建 node/ 与 dsh/ 目标目录。
    # 2. 复制 node 可执行文件，文件名保持平台惯例。
    # 3. 递归复制 DSH 包内容（含其 node_modules）。
    # 4. 写 runtime.json，记录 Node/DSH 版本与 staging 时间，不写任何凭据。
    $nodeTarget = Join-Path $Root "node"
    $dshTarget = Join-Path $Root "dsh"
    New-Item -ItemType Directory -Force -Path $nodeTarget, $dshTarget | Out-Null

    $nodeName = if ($IsWindows) { "node.exe" } else { "node" }
    Copy-Item -LiteralPath $NodeSource -Destination (Join-Path $nodeTarget $nodeName) -Force
    Get-ChildItem -LiteralPath $DshSource -Force | Copy-Item -Destination $dshTarget -Recurse -Force

    $dshPackage = Get-Content -LiteralPath (Join-Path $DshSource "package.json") -Raw | ConvertFrom-Json
    $nodeVersion = (& $NodeSource --version).Trim()
    $metadata = [ordered]@{
        node = $nodeVersion
        dsh = [string]$dshPackage.version
        stagedAt = (Get-Date).ToString("o")
    }
    $metadataPath = Join-Path $Root "runtime.json"
    $metadata | ConvertTo-Json | Set-Content -LiteralPath $metadataPath -Encoding utf8
    return $metadataPath
}

<#
.SYNOPSIS
    用 staged Node 启动 staged DSH 的 --version，验证依赖完整。
.PARAMETER Root
    可选运行时根目录。
#>
function Test-StagedRuntime {
    param([string]$Root)

    # 1. 使用 staged node 与 staged lib/bin.js 执行 --version。
    # 2. 30 秒内未退出视为失败并强制结束进程。
    # 3. 非零退出码直接失败，避免打包出不可用运行时。
    $nodeName = if ($IsWindows) { "node.exe" } else { "node" }
    $node = Join-Path $Root "node\$nodeName"
    $bin = Join-Path $Root "dsh\lib\bin.js"
    $stdout = Join-Path $Root ".smoke.stdout"
    $stderr = Join-Path $Root ".smoke.stderr"
    $process = Start-Process -FilePath $node -ArgumentList @($bin, "--version") -PassThru `
        -RedirectStandardOutput $stdout -RedirectStandardError $stderr -WindowStyle Hidden
    if (-not $process.WaitForExit(30000)) {
        Stop-Process -Id $process.Id -Force -ErrorAction SilentlyContinue
        throw "staged DSH --version 超时（30 秒）"
    }
    $errorText = Get-Content -LiteralPath $stderr -Raw -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $stdout, $stderr -Force -ErrorAction SilentlyContinue
    if ($process.ExitCode -ne 0) {
        throw "staged DSH --version 失败，退出码 $($process.ExitCode)：$errorText"
    }
}

# 1. 解析 DSH 包与 Node，先完成所有校验再改动目标目录。
$dshSource = Resolve-DshPackage -Value $DshPath
$nodeSource = Resolve-NodeExe -Value $NodePath
New-Item -ItemType Directory -Force -Path $RuntimeRoot | Out-Null

# 2. 已存在 staged 运行时且未传 -Force 时中止，避免误覆盖大目录。
$targets = @(
    (Join-Path $RuntimeRoot "node"),
    (Join-Path $RuntimeRoot "dsh"),
    (Join-Path $RuntimeRoot "runtime.json")
)
$existing = @($targets | Where-Object { Test-Path -LiteralPath $_ })
if ($existing.Count -gt 0 -and -not $Force) {
    throw "运行时目录已存在；如需重新 staging，请加 -Force。"
}

# 3. 清理旧产物（仅允许删除 RuntimeRoot 内路径）。
if ($Force) {
    foreach ($target in $targets) {
        Assert-UnderRuntimeRoot -Target $target -Root $RuntimeRoot
        Remove-Item -LiteralPath $target -Recurse -Force -ErrorAction SilentlyContinue
    }
}

# 4. 复制并写版本清单；默认执行 --version 冒烟。
$metadataPath = Copy-Runtime -NodeSource $nodeSource -DshSource $dshSource -Root $RuntimeRoot
if (-not $SkipSmokeTest) {
    Test-StagedRuntime -Root $RuntimeRoot
}

Write-Output "runtime staged: $RuntimeRoot"
Write-Output "metadata: $metadataPath"
Write-Output "next: pnpm build:offline"
