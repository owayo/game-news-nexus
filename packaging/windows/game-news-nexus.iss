; Game News Nexus の Windows 用インストーラー (Inno Setup 6.3 以降。7 でもコンパイルできる)。
;
; scripts/build-installer.ps1 (mise run installer) が、版と実行ファイルを渡してコンパイルする:
;   ISCC.exe /DAppVersion=<版> /DSourceExe=<game-news-nexus.exe> /O<出力先> /F<名前> game-news-nexus.iss
;
; - 管理者権限は要らない。利用者ごとに %LOCALAPPDATA%\Programs\Game News Nexus へ入れる。
; - スタートメニューのショートカットは、アプリが通知のために自分で作るもの (crates/gnn-app/src/shortcut.rs) と
;   同じ場所・同じ名前・同じ AppUserModelID にする。アプリは、同じ実行ファイルを指すショートカットがあれば
;   作り直さないので二重にならない。ID がずれると通知が出なくなるため、build-installer.ps1 が下の値を
;   アプリのコードと照らしてからコンパイルする。
; - 更新とアンインストールでは、動いているアプリに、入っている実行ファイルの --quit で通常の終了 (定期取得を
;   止め、ログを書き切る) を頼み、プロセスが消えるまで待つ。トレイだけで常駐しているアプリには閉じるウィンドウが
;   なく、Restart Manager や taskkill では強制終了になるため。画面を出しているときは先に確かめ、黙っているとき
;   (/SILENT・/VERYSILENT) は確かめない。終わらなければ今までの方法に戻す (更新は Restart Manager で閉じ
;   (CloseApplications=force)、アンインストールは taskkill で終わらせる)。
;   黙って更新したときは、動いていたアプリをトレイで起動し直す。
; - アンインストールでは、この場所を指す自動起動の登録 (HKCU の Run) も消す。設定と記事のデータ
;   (%APPDATA%\game-news-nexus など) は消さない。

#ifndef AppVersion
  #error AppVersion がありません。scripts/build-installer.ps1 (mise run installer) からコンパイルしてください
#endif
#ifndef SourceExe
  #error SourceExe がありません。scripts/build-installer.ps1 (mise run installer) からコンパイルしてください
#endif
#if Ver < EncodeVer(6, 3, 0, 0)
  #error Inno Setup 6.3 以降が要ります
#endif

; インストールの ID (アンインストールの登録のキーは <AppId>_is1)。公開した後は変えない
; (変えると更新が前の版を見つけられず、別のアプリとして二重に入る)
#define AppId "io.github.owayo.game-news-nexus"
; crates/gnn-app/src/shortcut.rs の APP_NAME と同じ (ショートカットの名前になる)
#define AppName "Game News Nexus"
#define AppExeName "game-news-nexus.exe"
; crates/gnn-app/src/shortcut.rs の APP_ID と同じ
#define AppUserModelId "io.github.owayo.game-news-nexus"
; crates/gnn-app/src/autostart.rs の VALUE_NAME と同じ
#define AutostartValueName "GameNewsNexus"
#define AppDescription "ゲームのお知らせを集めて読む"
#define AppPublisher "Yohei"
#define AppUrl "https://github.com/owayo/game-news-nexus"

[Setup]
AppId={#AppId}
AppName={#AppName}
AppVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion}
AppPublisher={#AppPublisher}
AppPublisherURL={#AppUrl}
AppSupportURL={#AppUrl}/issues
AppUpdatesURL={#AppUrl}/releases
AppCopyright=Copyright (c) 2026 {#AppPublisher}
VersionInfoVersion={#AppVersion}
VersionInfoProductName={#AppName}
VersionInfoDescription={#AppName} のセットアップ
; 利用者ごとに入れる。{autopf} は %LOCALAPPDATA%\Programs、{autoprograms} は利用者のスタートメニュー
PrivilegesRequired=lowest
DefaultDirName={autopf}\{#AppName}
DisableProgramGroupPage=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
; --quit で終わらなかったアプリ (PrepareToInstall) を Restart Manager で閉じ、閉じられなければ強制的に終わらせる。
; アプリは Restart Manager を登録していないので、閉じたアプリは Restart Manager では起動し直さない
CloseApplications=force
RestartApplications=no
SetupMutex={#AppId}.setup
UninstallDisplayName={#AppName}
UninstallDisplayIcon={app}\{#AppExeName}
SetupIconFile=..\..\crates\gnn-app\assets\icons\app.ico
#if Ver >= EncodeVer(6, 6, 0, 0)
WizardStyle=modern dynamic
#else
WizardStyle=modern
#endif
ShowLanguageDialog=no
Compression=lzma2/max
SolidCompression=yes
; 出力先と名前は build-installer.ps1 が /O と /F で指定する
OutputDir=..\..\target\dist
OutputBaseFilename=game-news-nexus-{#AppVersion}-windows-x64-setup

[Languages]
Name: "japanese"; MessagesFile: "compiler:Languages\Japanese.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[InstallDelete]
; 前の版のライセンスを消してから入れ直す (なくなったファイルを残さない)
Type: filesandordirs; Name: "{app}\licenses"

[Files]
Source: "{#SourceExe}"; DestDir: "{app}"; DestName: "{#AppExeName}"; Flags: ignoreversion
Source: "..\..\LICENSE"; DestDir: "{app}"; Flags: ignoreversion
; 第三者のライセンス (Rust のクレートの一覧 THIRD_PARTY_LICENSES.md・IPAdic の辞書など。中身はこのフォルダの README.md)
Source: "..\..\crates\gnn-app\assets\licenses\*"; DestDir: "{app}\licenses"; Flags: ignoreversion recursesubdirs

[Icons]
Name: "{autoprograms}\{#AppName}"; Filename: "{app}\{#AppExeName}"; WorkingDir: "{app}"; Comment: "{#AppDescription}"; AppUserModelID: "{#AppUserModelId}"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\{#AppExeName}"; WorkingDir: "{app}"; Comment: "{#AppDescription}"; AppUserModelID: "{#AppUserModelId}"; Tasks: desktopicon

[Run]
Filename: "{app}\{#AppExeName}"; Description: "{cm:LaunchProgram,{#AppName}}"; Flags: nowait postinstall skipifsilent
Filename: "{app}\{#AppExeName}"; Parameters: "--hidden"; Flags: nowait; Check: ShouldRestartAfterSilentUpdate

[Code]
const
  RunKey = 'Software\Microsoft\Windows\CurrentVersion\Run';
  StartupApprovedKey = 'Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run';

var
  AppWasRunning: Boolean;

// {app} の実行ファイルで動いているプロセスを WMI で数える。Terminate なら強制的に終わらせる。
// 別の場所の game-news-nexus.exe (開発中のビルドなど) は数えない。調べられなければ -1
function CountAppProcesses(Terminate: Boolean): Integer;
var
  Locator, Service, Items, Item: Variant;
  ItemCount, I, ProcessId, ResultCode: Integer;
  ExePath, ItemPath: String;
begin
  Result := 0;
  ExePath := ExpandConstant('{app}\{#AppExeName}');
  try
    Locator := CreateOleObject('WbemScripting.SWbemLocator');
    Service := Locator.ConnectServer('.', 'root\CIMV2');
    Items := Service.ExecQuery('SELECT ProcessId, ExecutablePath FROM Win32_Process WHERE Name = ''{#AppExeName}''');
    ItemCount := Items.Count;
    for I := 0 to ItemCount - 1 do
    begin
      Item := Items.ItemIndex(I);
      // ほかの利用者のプロセスなど、場所を読めないものは数えない
      if not VarIsNull(Item.ExecutablePath) then
      begin
        ItemPath := Item.ExecutablePath;
        if SameText(ItemPath, ExePath) then
        begin
          Result := Result + 1;
          if Terminate then
          begin
            ProcessId := Item.ProcessId;
            Log(Format('%s (PID %d) を終わらせます', [ItemPath, ProcessId]));
            Exec(ExpandConstant('{sys}\taskkill.exe'), '/F /PID ' + IntToStr(ProcessId), '', SW_HIDE, ewWaitUntilTerminated, ResultCode);
          end;
        end;
      end;
    end;
  except
    Log('動いているアプリを調べられません: ' + GetExceptionMessage);
    Result := -1;
  end;
end;

// 終了を頼んだ・強制的に終わらせたアプリが消えるまで待つ (最大 Seconds 秒)。消えたら True。
// 途中で WMI が使えなくなったときは、確かめようがないので True にして進める (それでもファイルが使用中なら、
// 更新は Restart Manager が閉じ、アンインストーラーは消せなかったファイルを知らせる)
function WaitForAppExit(Seconds: Integer): Boolean;
var
  I, Running: Integer;
begin
  for I := 1 to Seconds * 4 do
  begin
    Running := CountAppProcesses(False);
    if Running <= 0 then
    begin
      if Running < 0 then
        Log('アプリが終わったかを確かめられないまま進めます');
      Result := True;
      Exit;
    end;
    Sleep(250);
  end;
  Result := False;
end;

// 入っている実行ファイルに --quit を渡し、動いているアプリに通常の終了を頼む。動いているプロセスと同じ
// 実行ファイルなので、頼み方は必ず通じる。--quit はアプリのプロセスが終わるまで待ってから戻り (どの段にも
// 期限があり、最大 30 秒ほど)、終わったか起動していなければ 0、伝えられない・終わらない・別の場所の実行
// ファイルで起動している (終わらせない) ときは 1 を返す
function AskAppToQuit(): Boolean;
var
  ExePath: String;
  ResultCode: Integer;
begin
  Result := False;
  ExePath := ExpandConstant('{app}\{#AppExeName}');
  if not FileExists(ExePath) then
  begin
    Log('終了を頼む実行ファイルがありません: ' + ExePath);
    Exit;
  end;
  Log('動いているアプリに終了を頼みます: "' + ExePath + '" --quit');
  if not Exec(ExePath, '--quit', '', SW_HIDE, ewWaitUntilTerminated, ResultCode) then
  begin
    Log('--quit を起動できません: ' + SysErrorMessage(ResultCode));
    Exit;
  end;
  Log(Format('--quit の終了コード: %d', [ResultCode]));
  Result := ResultCode = 0;
end;

// 動いているアプリに終了を頼み、プロセスが消えるまで待つ。消えたら True。
// --quit は、同じ実行ファイル (この場所) で起動したアプリだけを終わらせ、同じデータの場所を使う別の場所の
// アプリ (開発中のビルドなど) には触らずに 1 を返す。この場所のアプリが別のデータの場所 (GNN_HOME) を
// 使っていて --quit が届かないときは、ここで待ってから今までの方法に戻す
function QuitRunningApp(): Boolean;
begin
  // --quit が失敗したら待たずに戻る (and の右辺を必ず評価しないとは限らないので、分けて書く)
  Result := AskAppToQuit();
  if Result then
    Result := WaitForAppExit(10);
  if Result then
    Log('動いていたアプリが終わりました')
  else
    Log('終了を頼んでも、動いていたアプリが終わりません');
end;

// コマンドラインに Name (/NOCLOSEAPPLICATIONS など) があるか
function HasSwitch(const Name: String): Boolean;
var
  I: Integer;
begin
  Result := False;
  for I := 1 to ParamCount do
    if CompareText(ParamStr(I), Name) = 0 then
    begin
      Result := True;
      Exit;
    end;
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
var
  Running: Integer;
begin
  Result := '';
  // 黙って更新したときに起動し直すかどうかを決めるため、動いていたかを覚える (呼び直されたときに、
  // 終わらせた後の数え直しで忘れない)。WMI で調べられないとき (-1) は動いていなかったものとし、起動し直さない
  Running := CountAppProcesses(False);
  if Running > 0 then
    AppWasRunning := True;
  // 動いていれば、入れ替える前に通常の終了を頼む (この関数は Restart Manager が使用中のファイルを調べる前に
  // 呼ばれる)。終わらないとき・WMI で調べられないときは Restart Manager に任せる。/NOCLOSEAPPLICATIONS の
  // ときは、Restart Manager と同じく閉じない
  if (Running <= 0) or HasSwitch('/NOCLOSEAPPLICATIONS') then
    Exit;
  // 画面を出しているときは、Restart Manager のページの代わりにここで確かめる。黙って更新したときは、
  // Restart Manager と同じく確かめずに閉じる
  if not WizardSilent then
    if SuppressibleMsgBox('{#AppName} が起動しています。' + #13#10#13#10 + '「OK」を押すと {#AppName} を終了して、インストールを続けます。', mbConfirmation, MB_OKCANCEL, IDOK) <> IDOK then
    begin
      Result := '{#AppName} を終了しなかったので、インストールを取りやめました。';
      Exit;
    end;
  if not QuitRunningApp() then
    Log('Restart Manager で閉じます');
end;

function ShouldRestartAfterSilentUpdate(): Boolean;
begin
  Result := WizardSilent and AppWasRunning;
end;

// アンインストールの確認の後、ファイルを消す前に、動いているアプリを閉じる
procedure CloseRunningApp();
var
  Running: Integer;
begin
  Running := CountAppProcesses(False);
  if Running = 0 then
    Exit;
  if Running < 0 then
  begin
    // WMI で調べられないときは、利用者に閉じてもらう (メッセージを出さずに消すときは、そのまま進める)
    if SuppressibleMsgBox('{#AppName} が起動しているかを確かめられませんでした。' + #13#10#13#10 + '起動しているときは終了してから「OK」を押してください。', mbInformation, MB_OKCANCEL, IDOK) <> IDOK then
      Abort;
    Exit;
  end;
  // 黙って消すときは、黙った更新と同じく確かめずに終わらせる
  if not UninstallSilent then
    if SuppressibleMsgBox('{#AppName} が起動しています。' + #13#10#13#10 + '「OK」を押すと {#AppName} を終了して、アンインストールを続けます。', mbConfirmation, MB_OKCANCEL, IDOK) <> IDOK then
      Abort;
  // 通常の終了を頼み、終わらなければ強制的に終わらせる
  if QuitRunningApp() then
    Exit;
  CountAppProcesses(True);
  if not WaitForAppExit(15) then
  begin
    SuppressibleMsgBox('{#AppName} を終了できませんでした。終了してから、もう一度アンインストールしてください。', mbError, MB_OK, IDOK);
    Abort;
  end;
end;

// 自動起動の登録のコマンド ("<実行ファイル>" --hidden) から、実行ファイルのパスを取り出す
function CommandExecutable(const Command: String): String;
var
  Rest: String;
  Last: Integer;
begin
  Rest := Trim(Command);
  if (Length(Rest) > 0) and (Rest[1] = '"') then
  begin
    Delete(Rest, 1, 1);
    Last := Pos('"', Rest);
  end
  else
    Last := Pos(' ', Rest);
  if Last > 0 then
    Result := Copy(Rest, 1, Last - 1)
  else
    Result := Rest;
end;

// 自動起動の登録 (アプリの設定で作る HKCU の Run) が、この場所の実行ファイルを指していれば消す
procedure RemoveAutostart();
var
  Command: String;
begin
  if not RegQueryStringValue(HKEY_CURRENT_USER, RunKey, '{#AutostartValueName}', Command) then
    Exit;
  if not SameText(CommandExecutable(Command), ExpandConstant('{app}\{#AppExeName}')) then
  begin
    Log('自動起動の登録は別の場所を指しているので残します: ' + Command);
    Exit;
  end;
  RegDeleteValue(HKEY_CURRENT_USER, RunKey, '{#AutostartValueName}');
  // タスク マネージャーで無効にしたときの記録
  RegDeleteValue(HKEY_CURRENT_USER, StartupApprovedKey, '{#AutostartValueName}');
  Log('自動起動の登録を消しました');
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  case CurUninstallStep of
    usAppMutexCheck: CloseRunningApp();
    usUninstall: RemoveAutostart();
  end;
end;
