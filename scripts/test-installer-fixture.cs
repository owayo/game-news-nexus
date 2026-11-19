// インストーラーの検査 (scripts/test-installer-windows.ps1) で、アプリ (game-news-nexus.exe) の代わりに入れる。
// .NET Framework の csc.exe で x64 の実行ファイルにする (Rust のビルドを待たずに検査するため)。本物と同じく
// コンソールを持たない GUI のアプリ (/target:winexe) として作る。
//
// アプリの --quit と同じ約束で動く: 起動中の代わりに終了を頼み、終わるまで待ってから戻る。
// 起動していなければ何もせずに 0、終わったら 0、頼んでも時間内に終わらなければ 1。
//
//   game-news-nexus.exe serve      終了を頼まれたら終わる (アプリの通常の終了にあたる)
//   game-news-nexus.exe --hidden   serve と同じ (黙った更新の後に、インストーラーが起動し直すとき)
//   game-news-nexus.exe stubborn   終了を頼まれても終わらない (インストーラーが今の方法に戻るかを確かめる)
//   game-news-nexus.exe --quit     起動中の代わりに終了を頼み、終わるまで最大 5 秒待つ
//
// 環境変数 GNN_FIXTURE_LOG があれば、そのファイルに起動 ("start <動き>") と、頼まれて終わったこと
// ("quit <動き>") を書き足す。Restart Manager や taskkill に終わらされたときは "quit" を書かない。
// --quit は結果を "ask done" (終わった)・"ask none" (起動していない)・"ask timeout" (終わらない) で書く。
// C# 5 (csc.exe の対応する版) で書く。

using System;
using System.IO;
using System.Text;
using System.Threading;

static class Fixture
{
    // 名前は同じセッションのプロセスだけに見える (Local\)
    const string QuitEventName = @"Local\io.github.owayo.game-news-nexus.installer-test.quit";
    const string RunningMutexName = @"Local\io.github.owayo.game-news-nexus.installer-test.running";

    static int Main(string[] args)
    {
        string mode = args.Length > 0 ? args[0] : "";
        switch (mode)
        {
            case "serve":
            case "--hidden":
                return Serve(mode, true);
            case "stubborn":
                return Serve(mode, false);
            case "--quit":
                return Quit();
            default:
                Console.Error.WriteLine("不明な引数です: " + mode);
                return 2;
        }
    }

    // 起動中の印 (ミューテックス) を持ち、終了の依頼 (イベント) を待つ。
    static int Serve(string mode, bool honorQuit)
    {
        bool created;
        using (Mutex running = new Mutex(true, RunningMutexName, out created))
        {
            if (!created)
            {
                Log("busy " + mode);
                return 3;
            }
            try
            {
                using (EventWaitHandle quit = new EventWaitHandle(false, EventResetMode.ManualReset, QuitEventName))
                {
                    // 前の代わりへの依頼が残っていても、この代わりへの依頼とはみなさない
                    quit.Reset();
                    Log("start " + mode);
                    // 検査が途中で止まっても残らないよう、10 分で終わる
                    bool asked = quit.WaitOne(TimeSpan.FromMinutes(10));
                    if (!asked)
                    {
                        return 1;
                    }
                    if (!honorQuit)
                    {
                        Thread.Sleep(TimeSpan.FromMinutes(10));
                        return 1;
                    }
                    Log("quit " + mode);
                    return 0;
                }
            }
            finally
            {
                running.ReleaseMutex();
            }
        }
    }

    // 起動中の代わりに終了を頼み、起動中の印が離れるまで待つ。
    static int Quit()
    {
        EventWaitHandle quit;
        if (!EventWaitHandle.TryOpenExisting(QuitEventName, out quit))
        {
            Log("ask none");
            return 0;
        }
        using (quit)
        {
            quit.Set();
        }
        Mutex running;
        if (!Mutex.TryOpenExisting(RunningMutexName, out running))
        {
            Log("ask done");
            return 0;
        }
        using (running)
        {
            bool acquired;
            try
            {
                acquired = running.WaitOne(TimeSpan.FromSeconds(5));
            }
            catch (AbandonedMutexException)
            {
                // 離さずに終わった (強制的に終わらされた)
                acquired = true;
            }
            if (!acquired)
            {
                Log("ask timeout");
                return 1;
            }
            running.ReleaseMutex();
            Log("ask done");
            return 0;
        }
    }

    static void Log(string line)
    {
        string path = Environment.GetEnvironmentVariable("GNN_FIXTURE_LOG");
        if (string.IsNullOrEmpty(path))
        {
            return;
        }
        byte[] bytes = new UTF8Encoding(false).GetBytes(line + "\r\n");
        // 検査のスクリプトが読んでいる最中でも書けるよう、共有して開く。ほかのプログラムと重なったら少し待つ
        for (int attempt = 0; ; attempt++)
        {
            try
            {
                using (FileStream stream = new FileStream(path, FileMode.Append, FileAccess.Write, FileShare.ReadWrite | FileShare.Delete))
                {
                    stream.Write(bytes, 0, bytes.Length);
                }
                return;
            }
            catch (IOException)
            {
                if (attempt >= 50)
                {
                    throw;
                }
                Thread.Sleep(20);
            }
        }
    }
}
