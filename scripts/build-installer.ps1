<#
.SYNOPSIS
Inno Setup で Windows のインストーラー (setup.exe) を作る (mise run installer)。

.DESCRIPTION
packaging/windows/game-news-nexus.iss をコンパイルして、<出力先>\game-news-nexus-<版>-windows-x64-setup.exe を作る。
版は Cargo.toml の [workspace.package] の version。実行ファイルは先に作っておく
(mise run release、または cargo build --release -p gnn-app --target <ターゲット>)。

インストーラーが作るショートカットの名前と AppUserModelID、アンインストールで消す自動起動の登録名は、
アプリのコード (crates/gnn-app/src/shortcut.rs・autostart.rs) と同じでなければならない
(ずれると通知が出なくなる・登録が残る)。コンパイルの前に照らして、違えば止まる。

第三者のライセンス (crates/gnn-app/assets/licenses) は、フォルダごと {app}\licenses に入れる。実行ファイルに
組み込んだ Rust のクレートのライセンスの一覧 (THIRD_PARTY_LICENSES.md。mise run licenses が作り、CI が古くないかを
確かめる) がなければ止まる。

実行ファイルは C ランタイムを静的にリンクしたもの (-C target-feature=+crt-static) でなければならない。
既定の MSVC のビルドは VCRUNTIME140.dll を使うが、利用者ごとのインストーラーは管理者の権限が要る
Visual C++ 再頒布可能パッケージを入れられず、入っていない PC では起動できないため。

Inno Setup 6.3 以降が要る。環境変数 ISCC・PATH・Inno Setup の入れ先の順に ISCC.exe を探し、なければ止まる。
Windows PowerShell 5.1 でも動く書き方に限る (日本語を含むので UTF-8 の BOM 付きで保存する)。

.PARAMETER Target
cargo build --target に渡したターゲット (例: x86_64-pc-windows-msvc)。<target>\<ターゲット>\release の
実行ファイルを使う。

.PARAMETER ExecutablePath
入れる実行ファイル。既定は <target>\release\game-news-nexus.exe (Target を指定したときは上のとおり)。
<target> は cargo metadata の target_directory (CARGO_TARGET_DIR などで場所を変えていても追う)。

.PARAMETER OutputDirectory
setup.exe を置くフォルダ。既定は <target>\dist。

.PARAMETER AllowDynamicCrt
VCRUNTIME140.dll などを使う実行ファイルでも作る (手元でインストーラーを試すとき用。配布には使わない)。
#>
param(
    [string] $Target = '',
    [string] $ExecutablePath = '',
    [string] $OutputDirectory = '',
    [switch] $AllowDynamicCrt
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version 3.0

$root = Split-Path -Parent $PSScriptRoot
$issPath = Join-Path $root 'packaging\windows\game-news-nexus.iss'
$licensesPath = Join-Path $root 'crates\gnn-app\assets\licenses'

# mise や CI が出力を受け取るときは、日本語を UTF-8 で渡す (Windows PowerShell の既定はコンソールのコードページ)。
# コンソールのコードページも変わるので、最後に戻す
$previousEncoding = $null
if ([Console]::IsOutputRedirected -or [Console]::IsErrorRedirected) {
    try {
        $previousEncoding = [Console]::OutputEncoding
        [Console]::OutputEncoding = New-Object System.Text.UTF8Encoding($false)
    } catch {
        $previousEncoding = $null
    }
}

function Stop-Build([string] $Message) {
    [Console]::Error.WriteLine($Message)
    exit 1
}

function Read-Text([string] $Path) {
    return [IO.File]::ReadAllText($Path, [Text.Encoding]::UTF8)
}

# ファイルの SHA-256 (16 進の小文字)。Get-FileHash は使わない。Windows PowerShell 5.1 では Get-FileHash が
# モジュール (Microsoft.PowerShell.Utility の psm1) の関数で、PowerShell 7 から mise などを挟んで起動されると
# 7 のモジュールの場所 (PSModulePath) を受け継いで読み込めず、「認識されません」で止まるため
function Get-Sha256([string] $Path) {
    $stream = [IO.File]::OpenRead($Path)
    try {
        $sha = [Security.Cryptography.SHA256]::Create()
        try {
            return [BitConverter]::ToString($sha.ComputeHash($stream)).Replace('-', '').ToLowerInvariant()
        } finally {
            $sha.Dispose()
        }
    } finally {
        $stream.Dispose()
    }
}

# Text から Pattern の 1 つ目のグループを取り出す。なければ止まる
function Get-Match([string] $Text, [string] $Pattern, [string] $What) {
    $match = [regex]::Match($Text, $Pattern, [Text.RegularExpressions.RegexOptions]::Multiline)
    if (-not $match.Success) {
        Stop-Build "$What が見つかりません"
    }
    return $match.Groups[1].Value
}

function Get-WorkspaceVersion {
    $inPackage = $false
    foreach ($line in [IO.File]::ReadAllLines((Join-Path $root 'Cargo.toml'))) {
        if ($line -match '^\s*\[') {
            $inPackage = $line -match '^\[workspace\.package\]\s*(#.*)?$'
            continue
        }
        if ($inPackage -and $line -match '^version\s*=\s*"([^"]*)"') {
            return $Matches[1]
        }
    }
    return ''
}

# cargo の出力先。cargo が使えなければ <リポジトリ>\target とみなす
function Get-TargetDirectory {
    if (Get-Command 'cargo' -CommandType Application -ErrorAction SilentlyContinue) {
        $previousPreference = $ErrorActionPreference
        $ErrorActionPreference = 'Continue'
        try {
            $json = & cargo metadata --format-version 1 --no-deps --manifest-path (Join-Path $root 'Cargo.toml') 2>$null
            if ($LASTEXITCODE -eq 0 -and $json) {
                $directory = ($json | Out-String | ConvertFrom-Json).target_directory
                if ($directory) {
                    return $directory
                }
            }
        } catch {
            # 下の既定の場所を使う
        } finally {
            $ErrorActionPreference = $previousPreference
        }
    }
    return (Join-Path $root 'target')
}

# PE の Machine (x64 は 0x8664) と、読み込む DLL の名前 (小文字)。PE でなければ $null
function Get-PeInfo([string] $Path) {
    $bytes = [IO.File]::ReadAllBytes($Path)
    if ($bytes.Length -lt 0x40 -or [BitConverter]::ToUInt16($bytes, 0) -ne 0x5A4D) {
        return $null
    }
    $pe = [BitConverter]::ToInt32($bytes, 0x3C)
    if ($pe -lt 0 -or $pe + 24 -gt $bytes.Length -or [BitConverter]::ToUInt32($bytes, $pe) -ne 0x00004550) {
        return $null
    }
    $machine = [int] [BitConverter]::ToUInt16($bytes, $pe + 4)
    $sectionCount = [BitConverter]::ToUInt16($bytes, $pe + 6)
    $optionalSize = [BitConverter]::ToUInt16($bytes, $pe + 20)
    $optional = $pe + 24
    # データディレクトリの位置は PE32+ (0x20B) と PE32 で違う。1 番目が import
    if ([BitConverter]::ToUInt16($bytes, $optional) -eq 0x20B) {
        $directories = $optional + 112
    } else {
        $directories = $optional + 96
    }
    $importRva = [BitConverter]::ToUInt32($bytes, $directories + 8)
    $sections = $optional + $optionalSize
    # RVA をファイルの中の位置にする。どのセクションにもなければ -1
    $toOffset = {
        param([uint32] $Rva)
        for ($i = 0; $i -lt $sectionCount; $i++) {
            $header = $sections + 40 * $i
            $virtualSize = [BitConverter]::ToUInt32($bytes, $header + 8)
            $virtualAddress = [BitConverter]::ToUInt32($bytes, $header + 12)
            $rawSize = [BitConverter]::ToUInt32($bytes, $header + 16)
            $rawPointer = [BitConverter]::ToUInt32($bytes, $header + 20)
            if ($Rva -ge $virtualAddress -and $Rva -lt $virtualAddress + [Math]::Max($virtualSize, $rawSize)) {
                return [int64] $Rva - $virtualAddress + $rawPointer
            }
        }
        return [int64] -1
    }
    $imports = New-Object 'System.Collections.Generic.List[string]'
    if ($importRva -ne 0) {
        # IMAGE_IMPORT_DESCRIPTOR (20 バイト) の並び。Name (12 バイト目) が 0 の項目で終わる
        $descriptor = & $toOffset $importRva
        while ($descriptor -ge 0 -and $descriptor + 20 -le $bytes.Length) {
            $nameRva = [BitConverter]::ToUInt32($bytes, $descriptor + 12)
            if ($nameRva -eq 0) {
                break
            }
            $start = & $toOffset $nameRva
            if ($start -lt 0) {
                break
            }
            $end = $start
            while ($end -lt $bytes.Length -and $bytes[$end] -ne 0) {
                $end++
            }
            $imports.Add([Text.Encoding]::ASCII.GetString($bytes, $start, $end - $start).ToLowerInvariant())
            $descriptor += 20
        }
    }
    return @{ Machine = $machine; Imports = $imports.ToArray() }
}

function Find-Iscc {
    if ($env:ISCC) {
        if (Test-Path -LiteralPath $env:ISCC -PathType Leaf) {
            return (Resolve-Path -LiteralPath $env:ISCC).Path
        }
        Stop-Build "環境変数 ISCC の場所に ISCC.exe がありません: $env:ISCC"
    }
    $command = Get-Command 'ISCC.exe' -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($command) {
        return $command.Source
    }
    $candidates = New-Object 'System.Collections.Generic.List[string]'
    # Inno Setup のアンインストールの登録にある入れ先 (利用者ごとに入れた場合も含む)
    foreach ($name in 'Inno Setup 7_is1', 'Inno Setup 6_is1') {
        foreach ($base in @(
                'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall',
                'HKLM:\Software\Microsoft\Windows\CurrentVersion\Uninstall',
                'HKLM:\Software\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall')) {
            $item = Get-ItemProperty -LiteralPath (Join-Path $base $name) -Name 'InstallLocation' -ErrorAction SilentlyContinue
            if ($item -and $item.InstallLocation) {
                $candidates.Add((Join-Path $item.InstallLocation 'ISCC.exe'))
            }
        }
    }
    $bases = @(${env:ProgramFiles(x86)}, $env:ProgramFiles)
    if ($env:LOCALAPPDATA) {
        $bases += Join-Path $env:LOCALAPPDATA 'Programs'
    }
    foreach ($base in $bases) {
        if (-not $base) {
            continue
        }
        foreach ($name in 'Inno Setup 7', 'Inno Setup 6') {
            $candidates.Add((Join-Path $base "$name\ISCC.exe"))
        }
    }
    foreach ($candidate in $candidates) {
        if (Test-Path -LiteralPath $candidate -PathType Leaf) {
            return $candidate
        }
    }
    return $null
}

try {
    $version = Get-WorkspaceVersion
    if ($version -notmatch '^(\d+)\.(\d+)\.(\d+)$') {
        Stop-Build "Cargo.toml の [workspace.package] の version を読めません: '$version'"
    }
    # 実行ファイルのバージョン情報 (VersionInfoVersion) は各部分が 65535 まで
    foreach ($part in @($Matches[1], $Matches[2], $Matches[3])) {
        if ([int64] $part -gt 65535) {
            Stop-Build "版 $version は Windows のバージョン情報に入りません (各部分は 65535 まで)"
        }
    }

    # インストーラーの値がアプリのコードと同じか
    $iss = Read-Text $issPath
    $shortcutSource = Read-Text (Join-Path $root 'crates\gnn-app\src\shortcut.rs')
    $autostartSource = Read-Text (Join-Path $root 'crates\gnn-app\src\autostart.rs')
    $checks = @(
        @{
            What = '通知に使うショートカットの AppUserModelID'
            Installer = Get-Match $iss '^#define AppUserModelId "([^"]+)"' 'game-news-nexus.iss の AppUserModelId'
            App = Get-Match $shortcutSource 'pub const APP_ID: &str = "([^"]+)";' 'crates/gnn-app/src/shortcut.rs の APP_ID'
            Source = 'crates/gnn-app/src/shortcut.rs の APP_ID'
        },
        @{
            What = 'ショートカットの名前'
            Installer = Get-Match $iss '^#define AppName "([^"]+)"' 'game-news-nexus.iss の AppName'
            App = Get-Match $shortcutSource 'pub const APP_NAME: &str = "([^"]+)";' 'crates/gnn-app/src/shortcut.rs の APP_NAME'
            Source = 'crates/gnn-app/src/shortcut.rs の APP_NAME'
        },
        @{
            What = '自動起動の登録名'
            Installer = Get-Match $iss '^#define AutostartValueName "([^"]+)"' 'game-news-nexus.iss の AutostartValueName'
            App = Get-Match $autostartSource 'const VALUE_NAME: &str = "([^"]+)";' 'crates/gnn-app/src/autostart.rs の VALUE_NAME'
            Source = 'crates/gnn-app/src/autostart.rs の VALUE_NAME'
        }
    )
    foreach ($check in $checks) {
        if ($check.Installer -cne $check.App) {
            Stop-Build ("インストーラーの{0} ({1}) が {2} ({3}) と違います。packaging/windows/game-news-nexus.iss を同じ値にしてください" -f $check.What, $check.Installer, $check.Source, $check.App)
        }
    }

    if (-not $ExecutablePath) {
        $targetDirectory = Get-TargetDirectory
        if ($Target) {
            $ExecutablePath = Join-Path $targetDirectory "$Target\release\game-news-nexus.exe"
        } else {
            $ExecutablePath = Join-Path $targetDirectory 'release\game-news-nexus.exe'
        }
    }
    if (-not (Test-Path -LiteralPath $ExecutablePath -PathType Leaf)) {
        Stop-Build "実行ファイルがありません: $ExecutablePath (先に mise run release か cargo build --release -p gnn-app を実行してください)"
    }
    $executable = (Resolve-Path -LiteralPath $ExecutablePath).Path
    $peInfo = Get-PeInfo $executable
    if ($null -eq $peInfo -or $peInfo.Machine -ne 0x8664) {
        Stop-Build "x64 の Windows の実行ファイルではありません: $executable"
    }
    # Visual C++ のランタイム (vcruntime140.dll・vcruntime140_1.dll・msvcp140.dll など) を使っていないか
    $runtimeDlls = @($peInfo.Imports | Where-Object { $_ -match '^(vcruntime|msvcp)\d+(_\d+)?\.dll$' })
    if ($runtimeDlls.Count -gt 0) {
        $message = "実行ファイルが Visual C++ のランタイム ($($runtimeDlls -join ', ')) を使っています。入っていない PC では起動できないので、" +
            "C ランタイムを静的にリンクしてビルドしてください (.cargo/config.toml の '-C target-feature=+crt-static' が効くよう、" +
            "環境変数 RUSTFLAGS・CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS を外して mise run release)。"
        if (-not $AllowDynamicCrt) {
            Stop-Build ($message + ' 手元で試すだけなら -AllowDynamicCrt を付けます')
        }
        [Console]::Error.WriteLine('注意: ' + $message + ' (-AllowDynamicCrt を付けたので、このまま作ります)')
    }
    if (-not (Get-ChildItem -LiteralPath $licensesPath -File -ErrorAction SilentlyContinue)) {
        Stop-Build "同梱する第三者のライセンスがありません: $licensesPath"
    }
    $crateLicenses = Join-Path $licensesPath 'THIRD_PARTY_LICENSES.md'
    if (-not (Test-Path -LiteralPath $crateLicenses -PathType Leaf) -or (Get-Item -LiteralPath $crateLicenses).Length -eq 0) {
        Stop-Build "Rust のクレートのライセンスの一覧がありません: $crateLicenses (mise run licenses で作ってください)"
    }

    $iscc = Find-Iscc
    if (-not $iscc) {
        Stop-Build @'
Inno Setup 6.3 以降が見つかりません。次のどちらかで入れてから、もう一度実行してください。
  winget install --id JRSoftware.InnoSetup -e
  https://jrsoftware.org/isdl.php からインストーラーを取得して入れる
入れた場所が見つからないときは、環境変数 ISCC に ISCC.exe のパスを指定してください。
'@
    }

    if (-not $OutputDirectory) {
        $OutputDirectory = Join-Path (Get-TargetDirectory) 'dist'
    }
    $null = New-Item -ItemType Directory -Path $OutputDirectory -Force
    $output = (Resolve-Path -LiteralPath $OutputDirectory).Path
    $baseName = "game-news-nexus-$version-windows-x64-setup"
    $setup = Join-Path $output "$baseName.exe"

    Write-Output "版: $version"
    Write-Output "実行ファイル: $executable"
    Write-Output "Inno Setup: $iscc"
    # 前に作ったものが残っていると、失敗しても成功したように見えるので消しておく
    if (Test-Path -LiteralPath $setup) {
        Remove-Item -LiteralPath $setup -Force
    }
    & $iscc "/DAppVersion=$version" "/DSourceExe=$executable" "/O$output" "/F$baseName" $issPath
    if ($LASTEXITCODE -ne 0) {
        Stop-Build "インストーラーをコンパイルできません (ISCC の終了コード $LASTEXITCODE)"
    }
    if (-not (Test-Path -LiteralPath $setup -PathType Leaf)) {
        Stop-Build "インストーラーができていません: $setup"
    }
    $hash = Get-Sha256 $setup
    Write-Output "インストーラー: $setup"
    Write-Output "SHA-256: $hash"
} finally {
    if ($null -ne $previousEncoding) {
        try {
            [Console]::OutputEncoding = $previousEncoding
        } catch {
            # 戻せなくても続ける
        }
    }
}
