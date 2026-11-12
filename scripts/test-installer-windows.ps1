<#
.SYNOPSIS
Windows のインストーラーを実際に入れて、更新して、消して確かめる (CI 用。mise run test-installer)。

.DESCRIPTION
いまの利用者のスタートメニュー・HKCU のレジストリ・%LOCALAPPDATA%\Programs を書き換え、同じ AppId で入っている
Game News Nexus があればその登録も上書きしてしまうので、GitHub Actions の外では動かさない。

InstallerPath を指定しないときは、アプリの代わりを入れたインストーラーを scripts/build-installer.ps1 で作り、
次を確かめる (Rust のビルドは要らない)。アプリの代わりは scripts/test-installer-fixture.cs を .NET Framework の
csc.exe で x64 の実行ファイルにしたもので、アプリの --quit と同じ約束で動く。
  - 入れる: 既定の入れ先・ファイル・スタートメニューのショートカット (実行ファイルと AppUserModelID)・
    利用者ごとのアンインストールの登録
  - 起動中に更新する: 動いているアプリに --quit で通常の終了をさせてから入れ替え、黙った更新なので
    --hidden で起動し直す
  - 起動中に消す: アプリに --quit で通常の終了をさせ、この場所を指す自動起動の登録 (Run) を消して、全部を取り除く
  - --quit で終わらないアプリは、更新では Restart Manager が、アンインストールでは taskkill が終わらせる
  - 自動起動の登録が別の場所を指しているときは、消さずに残す
InstallerPath を指定したときは、そのインストーラー (本物のアプリ) で、入れる・入れた実行ファイルを --help と
--quit (起動していないので、データのディレクトリも作らずに 0 で終わる) で起動する・消すを確かめる
(Release のワークフローが使う)。

.PARAMETER InstallerPath
確かめるインストーラー。省略すると、上のとおりアプリの代わりを入れたものを作る。
#>
param(
    [string] $InstallerPath = ''
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version 3.0

$root = Split-Path -Parent $PSScriptRoot

$previousEncoding = $null
if ([Console]::IsOutputRedirected -or [Console]::IsErrorRedirected) {
    try {
        $previousEncoding = [Console]::OutputEncoding
        [Console]::OutputEncoding = New-Object System.Text.UTF8Encoding($false)
    } catch {
        $previousEncoding = $null
    }
}

# 関数の戻り値として受け取られないよう、パイプラインを通さずに書く
function Stop-Test([string] $Message) {
    if ($env:GITHUB_ACTIONS -eq 'true') {
        [Console]::Out.WriteLine("::error::$Message")
    }
    [Console]::Error.WriteLine("検査に失敗しました: $Message")
    exit 1
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

function Get-Define([string] $Text, [string] $Name) {
    $match = [regex]::Match($Text, '^#define ' + $Name + ' "([^"]+)"', [Text.RegularExpressions.RegexOptions]::Multiline)
    if (-not $match.Success) {
        Stop-Test "packaging/windows/game-news-nexus.iss に #define $Name がありません"
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

function Get-RegistryValue([string] $Key, [string] $Name) {
    $item = Get-ItemProperty -LiteralPath $Key -Name $Name -ErrorAction SilentlyContinue
    if ($null -eq $item) {
        return $null
    }
    return $item.$Name
}

function Test-SamePath([string] $A, [string] $B) {
    if (-not $A -or -not $B) {
        return $false
    }
    return [string]::Equals([IO.Path]::GetFullPath($A).TrimEnd('\'), [IO.Path]::GetFullPath($B).TrimEnd('\'), [StringComparison]::OrdinalIgnoreCase)
}

function Wait-Until([scriptblock] $Condition, [int] $Seconds, [string] $What) {
    $deadline = [DateTime]::UtcNow.AddSeconds($Seconds)
    while (-not (& $Condition)) {
        if ([DateTime]::UtcNow -gt $deadline) {
            Stop-Test "$Seconds 秒待っても $What"
        }
        Start-Sleep -Milliseconds 250
    }
}

# 引数をそのまま渡して起動し、終わるまで待って終了コードを返す
function Invoke-Program([string] $Path, [string] $Arguments, [int] $TimeoutSeconds = 300) {
    $info = New-Object System.Diagnostics.ProcessStartInfo
    $info.FileName = $Path
    $info.Arguments = $Arguments
    $info.UseShellExecute = $false
    $process = [Diagnostics.Process]::Start($info)
    try {
        if (-not $process.WaitForExit($TimeoutSeconds * 1000)) {
            $process.Kill()
            Stop-Test "$Path が $TimeoutSeconds 秒たっても終わりません"
        }
        return $process.ExitCode
    } finally {
        $process.Dispose()
    }
}

function Show-Log([string] $Path) {
    if (Test-Path -LiteralPath $Path) {
        Write-Output "---- $Path ----"
        Get-Content -LiteralPath $Path | Write-Output
    }
}

$iss = [IO.File]::ReadAllText((Join-Path $root 'packaging\windows\game-news-nexus.iss'), [Text.Encoding]::UTF8)
$appId = Get-Define $iss 'AppId'
$appName = Get-Define $iss 'AppName'
$exeName = Get-Define $iss 'AppExeName'
$appUserModelId = Get-Define $iss 'AppUserModelId'
$runValueName = Get-Define $iss 'AutostartValueName'
$version = Get-WorkspaceVersion

$installDir = Join-Path $env:LOCALAPPDATA "Programs\$appName"
$installedExe = Join-Path $installDir $exeName
$uninstaller = Join-Path $installDir 'unins000.exe'
$shortcut = Join-Path ([Environment]::GetFolderPath('Programs')) "$appName.lnk"
$uninstallKey = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\${appId}_is1"
$machineUninstallKeys = @(
    "HKLM:\Software\Microsoft\Windows\CurrentVersion\Uninstall\${appId}_is1",
    "HKLM:\Software\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall\${appId}_is1"
)
$runKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'
$startupApprovedKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run'
$work = $null
$fixtureLog = $null
$fixtureLogSeen = 0
# この検査が起動した (インストーラーに起動させた) アプリの代わり。途中で止まったときに、これだけを終わらせる
$ownedProcesses = New-Object 'System.Collections.Generic.List[System.Diagnostics.Process]'

function Install-App([string] $Setup, [string] $Name) {
    $log = Join-Path $work "$Name.log"
    Write-Output "インストール ($Name)"
    $code = Invoke-Program $Setup ('/VERYSILENT /SUPPRESSMSGBOXES /NORESTART /LOG="' + $log + '"')
    if ($code -ne 0) {
        Show-Log $log
        Stop-Test "インストール ($Name) が終了コード $code で終わりました"
    }
}

function Uninstall-App([string] $Name) {
    $log = Join-Path $work "$Name.log"
    Write-Output "アンインストール ($Name)"
    if (-not (Test-Path -LiteralPath $uninstaller -PathType Leaf)) {
        Stop-Test "アンインストーラーがありません: $uninstaller"
    }
    $code = Invoke-Program $uninstaller ('/VERYSILENT /SUPPRESSMSGBOXES /NORESTART /LOG="' + $log + '"')
    if ($code -ne 0) {
        Show-Log $log
        Stop-Test "アンインストール ($Name) が終了コード $code で終わりました"
    }
    # 実際に消すのは一時フォルダに写したアンインストーラー。最後にアンインストーラー自身を消して終わる
    Wait-Until { -not (Test-Path -LiteralPath $uninstaller) -and -not (Test-Path -LiteralPath $uninstallKey) } 60 'アンインストールが終わりません'
    # 入れたフォルダは、アンインストーラーを消した後に消える
    $deadline = [DateTime]::UtcNow.AddSeconds(10)
    while ((Test-Path -LiteralPath $installDir) -and [DateTime]::UtcNow -lt $deadline) {
        Start-Sleep -Milliseconds 250
    }
}

function Assert-Installed {
    foreach ($file in @($installedExe, (Join-Path $installDir 'LICENSE'), $uninstaller)) {
        if (-not (Test-Path -LiteralPath $file -PathType Leaf)) {
            Stop-Test "入っていません: $file"
        }
    }
    foreach ($license in Get-ChildItem -LiteralPath (Join-Path $root 'crates\gnn-app\assets\licenses') -File) {
        $installed = Join-Path $installDir ('licenses\' + $license.Name)
        if (-not (Test-Path -LiteralPath $installed -PathType Leaf)) {
            Stop-Test "第三者のライセンスが入っていません: $($license.Name)"
        }
        if ((Get-Sha256 $installed) -ne (Get-Sha256 $license.FullName)) {
            Stop-Test "第三者のライセンスの中身が違います: $($license.Name)"
        }
    }

    # アプリが通知のために作るものと同じ場所・名前・AppUserModelID のショートカット
    if (-not (Test-Path -LiteralPath $shortcut -PathType Leaf)) {
        Stop-Test "スタートメニューのショートカットがありません: $shortcut"
    }
    $link = (New-Object -ComObject WScript.Shell).CreateShortcut($shortcut)
    if (-not (Test-SamePath $link.TargetPath $installedExe)) {
        Stop-Test "ショートカットが入れた実行ファイルを指していません: $($link.TargetPath)"
    }
    $folder = (New-Object -ComObject Shell.Application).Namespace((Split-Path -Parent $shortcut))
    $actualId = [string] $folder.ParseName((Split-Path -Leaf $shortcut)).ExtendedProperty('System.AppUserModel.ID')
    if ($actualId -cne $appUserModelId) {
        Stop-Test "ショートカットの AppUserModelID が $appUserModelId ではありません: '$actualId'"
    }

    # 利用者ごと (HKCU) の登録だけで、管理者向けの場所 (HKLM) には登録しない
    if (-not (Test-Path -LiteralPath $uninstallKey)) {
        Stop-Test "アンインストールの登録がありません: $uninstallKey"
    }
    $displayName = Get-RegistryValue $uninstallKey 'DisplayName'
    if ($displayName -cne $appName) {
        Stop-Test "アンインストールの登録の名前が $appName ではありません: '$displayName'"
    }
    $displayVersion = Get-RegistryValue $uninstallKey 'DisplayVersion'
    if ($displayVersion -cne $version) {
        Stop-Test "アンインストールの登録の版が Cargo.toml の $version ではありません: '$displayVersion'"
    }
    $location = Get-RegistryValue $uninstallKey 'InstallLocation'
    if (-not (Test-SamePath $location $installDir)) {
        Stop-Test "既定の入れ先が $installDir ではありません: '$location'"
    }
    foreach ($key in $machineUninstallKeys) {
        if (Test-Path -LiteralPath $key) {
            Stop-Test "管理者向けの場所に登録されています: $key"
        }
    }
}

function Assert-Uninstalled {
    foreach ($path in @($installedExe, $uninstaller, $shortcut)) {
        if (Test-Path -LiteralPath $path) {
            Stop-Test "アンインストールの後に残っています: $path"
        }
    }
    if (Test-Path -LiteralPath $uninstallKey) {
        Stop-Test "アンインストールの登録が残っています: $uninstallKey"
    }
    if (Test-Path -LiteralPath $installDir) {
        $left = @(Get-ChildItem -LiteralPath $installDir -Recurse -Force)
        if ($left.Count -gt 0) {
            Stop-Test ("入れたフォルダにファイルが残っています: " + (($left | ForEach-Object { $_.FullName }) -join ', '))
        }
        [Console]::Out.WriteLine("::warning::アンインストールの後に空のフォルダが残っています: $installDir")
    }
}

# アプリの代わりが書き足す記録 (scripts/test-installer-fixture.cs) のうち、Clear-FixtureLog の後の行。起動すると
# "start <動き>"、--quit で頼まれて終わると "quit <動き>"、--quit で頼んだ側は "ask done" (終わった)・
# "ask none" (起動していない)・"ask timeout" (終わらない) を書く。代わりが書いている最中でも読めるよう、共有して開く
function Get-FixtureLog {
    if (-not $fixtureLog -or -not (Test-Path -LiteralPath $fixtureLog -PathType Leaf)) {
        return @()
    }
    $stream = New-Object IO.FileStream($fixtureLog, [IO.FileMode]::Open, [IO.FileAccess]::Read, ([IO.FileShare]::ReadWrite -bor [IO.FileShare]::Delete))
    try {
        $text = (New-Object IO.StreamReader($stream, (New-Object Text.UTF8Encoding($false)))).ReadToEnd()
    } finally {
        $stream.Dispose()
    }
    return @($text -split "`r`n" | Where-Object { $_ } | Select-Object -Skip $script:fixtureLogSeen)
}

# ここまでの記録を読み飛ばす (消すと、ほかのプログラムが開いているときに消し損なうことがある)
function Clear-FixtureLog {
    $script:fixtureLogSeen += @(Get-FixtureLog).Count
}

function Assert-FixtureLogged([string] $Line, [string] $Message) {
    $lines = @(Get-FixtureLog)
    if ($lines -notcontains $Line) {
        Stop-Test ("$Message (アプリの代わりの記録に「$Line」がありません: " + ($lines -join ' / ') + ')')
    }
}

function Assert-FixtureNotLogged([string] $Line, [string] $Message) {
    $lines = @(Get-FixtureLog)
    if ($lines -contains $Line) {
        Stop-Test ("$Message (アプリの代わりの記録: " + ($lines -join ' / ') + ')')
    }
}

# 入れた場所の実行ファイルで動いているプロセス
function Get-InstalledProcess {
    return @(Get-Process -Name ([IO.Path]::GetFileNameWithoutExtension($exeName)) -ErrorAction SilentlyContinue |
            Where-Object { Test-SamePath $_.Path $installedExe })
}

# アプリの代わりを、入れた場所から起動する。--quit の依頼を待ち受けるまで待つ
function Start-InstalledFixture([string] $Mode) {
    $info = New-Object System.Diagnostics.ProcessStartInfo
    $info.FileName = $installedExe
    $info.Arguments = $Mode
    $info.UseShellExecute = $false
    $info.CreateNoWindow = $true
    $process = [Diagnostics.Process]::Start($info)
    $ownedProcesses.Add($process)
    Wait-Until { $process.HasExited -or (@(Get-FixtureLog) -contains "start $Mode") } 30 "アプリの代わり ($Mode) が起動しません"
    if ($process.HasExited) {
        Stop-Test "アプリの代わり ($Mode) が起動しません (終了コード $($process.ExitCode))"
    }
    return $process
}

# インストーラーが起動し直したアプリの代わりを待つ
function Wait-InstalledFixture([string] $Mode, [string] $What) {
    Wait-Until { @(Get-FixtureLog) -contains "start $Mode" } 30 $What
    $processes = @(Get-InstalledProcess)
    foreach ($process in $processes) {
        $ownedProcesses.Add($process)
    }
    if ($processes.Count -ne 1) {
        Stop-Test "$What (入れた場所で動いているプロセスが $($processes.Count) 個です)"
    }
    return $processes[0]
}

function Assert-Stopped([Diagnostics.Process] $Process, [string] $When) {
    if (-not $Process.WaitForExit(30000)) {
        $Process.Kill()
        Stop-Test "$When に、動いていたアプリが閉じられていません"
    }
}

try {
    if ($env:GITHUB_ACTIONS -ne 'true') {
        Stop-Test 'この検査は、いまの利用者のスタートメニューとレジストリを書き換えるので、GitHub Actions でだけ動かします'
    }
    if ($version -notmatch '^\d+\.\d+\.\d+$') {
        Stop-Test "Cargo.toml の [workspace.package] の version を読めません: '$version'"
    }
    foreach ($path in @($installDir, $shortcut, $uninstallKey)) {
        if (Test-Path -LiteralPath $path) {
            Stop-Test "検査の前から残っています: $path"
        }
    }
    $work = Join-Path ([IO.Path]::GetTempPath()) ('gnn-installer-test-' + [Guid]::NewGuid().ToString('N'))
    $null = New-Item -ItemType Directory -Path $work

    if ($InstallerPath) {
        # 本物のアプリのインストーラー
        $setup = (Resolve-Path -LiteralPath $InstallerPath).Path
        Install-App $setup 'install'
        Assert-Installed
        $code = Invoke-Program $installedExe '--help' 60
        if ($code -ne 0) {
            Stop-Test "入れた実行ファイルを --help で起動すると、終了コード $code で終わりました"
        }
        # 起動していないときの --quit は何もせずに 0 で終わり、データのディレクトリも作らない
        # (アンインストーラーは、一度も起動していないアプリにも --quit を呼ぶ)
        $gnnHome = Join-Path $work 'gnn-home'
        $env:GNN_HOME = $gnnHome
        try {
            $code = Invoke-Program $installedExe '--quit' 60
        } finally {
            Remove-Item -LiteralPath 'Env:\GNN_HOME'
        }
        if ($code -ne 0) {
            Stop-Test "起動していないときに入れた実行ファイルを --quit で起動すると、終了コード $code で終わりました"
        }
        if (Test-Path -LiteralPath $gnnHome) {
            Stop-Test "起動していないときの --quit が、データのディレクトリを作りました: $gnnHome"
        }
        Uninstall-App 'uninstall'
        Assert-Uninstalled
        Write-Output "インストーラーの検査が通りました: $setup"
        exit 0
    }

    # アプリの代わりを作り、それを入れたインストーラーを作る。代わりは x64 で VC ランタイムを使わない
    # (build-installer.ps1 が確かめる)
    $csc = Join-Path $env:SystemRoot 'Microsoft.NET\Framework64\v4.0.30319\csc.exe'
    if (-not (Test-Path -LiteralPath $csc -PathType Leaf)) {
        Stop-Test "アプリの代わりを作る C# のコンパイラ (.NET Framework 4) がありません: $csc"
    }
    $fixture = Join-Path $work "fixture\$exeName"
    $null = New-Item -ItemType Directory -Path (Split-Path -Parent $fixture)
    & $csc /nologo /codepage:65001 /target:winexe /platform:x64 /optimize+ "/out:$fixture" (Join-Path $PSScriptRoot 'test-installer-fixture.cs')
    if ($LASTEXITCODE -ne 0) {
        Stop-Test "アプリの代わりをコンパイルできません (csc の終了コード $LASTEXITCODE)"
    }
    $fixtureLog = Join-Path $work 'fixture.log'
    # インストーラーが起動するアプリの代わり (--quit・起動し直し) にも渡る
    $env:GNN_FIXTURE_LOG = $fixtureLog
    & (Join-Path $PSScriptRoot 'build-installer.ps1') -ExecutablePath $fixture -OutputDirectory (Join-Path $work 'dist')
    if ($LASTEXITCODE -ne 0) {
        Stop-Test "インストーラーを作れません (終了コード $LASTEXITCODE)"
    }
    $setup = Join-Path $work "dist\game-news-nexus-$version-windows-x64-setup.exe"
    if (-not (Test-Path -LiteralPath $setup -PathType Leaf)) {
        Stop-Test "インストーラーができていません: $setup"
    }

    Install-App $setup 'install'
    Assert-Installed

    # 起動中に更新すると、--quit で通常の終了をさせてから入れ替える (Restart Manager に閉じさせない)。
    # 黙った更新なので、動いていたアプリを --hidden で起動し直す
    Clear-FixtureLog
    $running = Start-InstalledFixture 'serve'
    Install-App $setup 'update'
    Assert-Stopped $running '更新したとき'
    Assert-FixtureLogged 'ask done' '更新したとき、インストーラーが --quit で終了を頼んでいません'
    Assert-FixtureLogged 'quit serve' '更新したとき、アプリが --quit で終わっていません'
    Assert-Installed
    $restarted = Wait-InstalledFixture '--hidden' '黙って更新した後に、アプリが --hidden で起動し直されません'

    # 起動中に消すと、--quit で通常の終了をさせてから消す。この場所を指す自動起動の登録も消す
    $command = '"' + $installedExe + '" --hidden'
    # 既にあるキーに New-Item -Force を使うと値が消えるので、ないときだけ作る
    # (GitHub Actions の Windows のランナーの利用者には、Run のキーもはじめからない)
    foreach ($key in @($runKey, $startupApprovedKey)) {
        if (-not (Test-Path -LiteralPath $key)) {
            $null = New-Item -Path $key -Force
        }
    }
    New-ItemProperty -LiteralPath $runKey -Name $runValueName -Value $command -PropertyType String -Force | Out-Null
    New-ItemProperty -LiteralPath $startupApprovedKey -Name $runValueName -Value ([byte[]] @(2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0)) -PropertyType Binary -Force | Out-Null
    Clear-FixtureLog
    Uninstall-App 'uninstall'
    Assert-Stopped $restarted 'アンインストールしたとき'
    Assert-FixtureLogged 'quit --hidden' 'アンインストールしたとき、アプリが --quit で終わっていません'
    Assert-Uninstalled
    if ($null -ne (Get-RegistryValue $runKey $runValueName)) {
        Stop-Test 'この場所を指す自動起動の登録が残っています'
    }
    if ($null -ne (Get-RegistryValue $startupApprovedKey $runValueName)) {
        Stop-Test 'タスク マネージャーの自動起動の記録 (StartupApproved) が残っています'
    }

    # --quit で終わらないアプリは、更新では今までどおり Restart Manager が閉じる
    Install-App $setup 'reinstall'
    Assert-Installed
    Clear-FixtureLog
    $running = Start-InstalledFixture 'stubborn'
    Install-App $setup 'update-stubborn'
    Assert-Stopped $running '--quit で終わらないアプリを更新したとき'
    Assert-FixtureLogged 'ask timeout' '--quit で終わらないアプリを更新したとき、インストーラーが --quit で終了を頼んでいません'
    Assert-FixtureNotLogged 'quit stubborn' '--quit で終わらないはずのアプリが終わっています'
    Assert-Installed
    $restarted = Wait-InstalledFixture '--hidden' 'Restart Manager で閉じて黙って更新した後に、アプリが --hidden で起動し直されません'

    # 入れた実行ファイルの --quit で、起動中のアプリが終わるまで待つ
    Clear-FixtureLog
    $code = Invoke-Program $installedExe '--quit' 60
    if ($code -ne 0) {
        Stop-Test "入れた実行ファイルを --quit で起動すると、終了コード $code で終わりました"
    }
    Assert-Stopped $restarted '--quit で終了を頼んだとき'
    Assert-FixtureLogged 'quit --hidden' '--quit で終了を頼んだとき、アプリが終わっていません'

    # --quit で終わらないアプリは、アンインストールでは今までどおり taskkill が終わらせる
    Clear-FixtureLog
    $running = Start-InstalledFixture 'stubborn'
    Uninstall-App 'uninstall-stubborn'
    Assert-Stopped $running '--quit で終わらないアプリをアンインストールしたとき'
    Assert-FixtureLogged 'ask timeout' '--quit で終わらないアプリをアンインストールしたとき、アンインストーラーが --quit で終了を頼んでいません'
    Assert-Uninstalled

    # 自動起動の登録が別の実行ファイル (開発中のビルドなど) を指しているときは残す。
    # 入れた実行ファイルのパスで始まる別のファイルも、別のものとして扱う
    Install-App $setup 'reinstall-elsewhere'
    Assert-Installed
    $elsewhere = '"' + $installedExe + '.old.exe" --hidden'
    New-ItemProperty -LiteralPath $runKey -Name $runValueName -Value $elsewhere -PropertyType String -Force | Out-Null
    Uninstall-App 'uninstall-elsewhere'
    Assert-Uninstalled
    if ((Get-RegistryValue $runKey $runValueName) -cne $elsewhere) {
        Stop-Test '別の場所を指す自動起動の登録が消えています'
    }
    Remove-ItemProperty -LiteralPath $runKey -Name $runValueName

    Write-Output "インストーラーの検査が通りました ($version)"
} finally {
    # 途中で止まったときに、この検査が起動したアプリの代わりを残さない (ほかのプロセスには触らない)
    foreach ($process in $ownedProcesses) {
        try {
            if (-not $process.HasExited) {
                $process.Kill()
            }
        } catch {
            # 終わっていれば何もしない
        }
    }
    if (Test-Path -LiteralPath 'Env:\GNN_FIXTURE_LOG') {
        Remove-Item -LiteralPath 'Env:\GNN_FIXTURE_LOG'
    }
    if ($work -and (Test-Path -LiteralPath $work)) {
        Remove-Item -LiteralPath $work -Recurse -Force -ErrorAction SilentlyContinue
    }
    if ($null -ne $previousEncoding) {
        try {
            [Console]::OutputEncoding = $previousEncoding
        } catch {
            # 戻せなくても続ける
        }
    }
}
