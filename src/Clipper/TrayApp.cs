using System.Diagnostics;
using System.Drawing.Drawing2D;
using System.Text.Json.Nodes;

namespace Clipper;

sealed class TrayApp : ApplicationContext
{
    const string ValorantSource = "VALORANT", AnyGameSource = "Any fullscreen game", DesktopSource = "Desktop (fallback)";

    readonly Config cfg;
    readonly SynchronizationContext ui;
    readonly NotifyIcon tray;
    readonly ToolStripMenuItem statusItem, queueItem, pauseItem;
    readonly Icon idleIcon = Dot(Color.FromArgb(140, 140, 140)),
                  recordingIcon = Dot(Color.FromArgb(230, 40, 40)),
                  warnIcon = Dot(Color.FromArgb(240, 170, 0));
    readonly ObsClient obs;
    readonly GameDetector games;
    readonly ClipShipper shipper;
    readonly HotkeyWindow? hotkey;
    readonly CancellationTokenSource stop = new();
    readonly object pendingLock = new();
    readonly List<PendingSave> pendingSaves = new();

    volatile bool paused;
    volatile bool bufferActive;
    volatile string activeGame = "";
    bool weStarted;                 // only stop a buffer this app started (or when paused)
    bool? valorantSourceShown;      // last visibility applied to the VALORANT / any-fullscreen sources
    bool monitorChecked;            // display capture points at a monitor that exists (per connection)
    DateTime lastGameActive = DateTime.MinValue;
    DateTime lastObsLaunch = DateTime.MinValue;

    sealed record PendingSave(string Game, DateTime When);

    public TrayApp(Config cfg)
    {
        this.cfg = cfg;
        ui = SynchronizationContext.Current!;

        statusItem = new ToolStripMenuItem("Starting…") { Enabled = false };
        var saveItem = new ToolStripMenuItem($"Save clip ({cfg.Hotkey})", null, (_, _) => SaveClip());
        queueItem = new ToolStripMenuItem("Open queue folder", null, (_, _) => Open(Paths.Queue));
        var marqueeItem = new ToolStripMenuItem("Open marquee", null, (_, _) => Open(cfg.MarqueeUrl));
        var logItem = new ToolStripMenuItem("Open log", null, (_, _) => Open(Paths.Log));
        pauseItem = new ToolStripMenuItem("Pause clipping", null, (_, _) => TogglePause());
        var quitItem = new ToolStripMenuItem("Quit", null, (_, _) => Quit());

        var menu = new ContextMenuStrip();
        menu.Items.AddRange(new ToolStripItem[]
        {
            statusItem, saveItem, new ToolStripSeparator(),
            queueItem, marqueeItem, logItem, new ToolStripSeparator(),
            pauseItem, quitItem,
        });
        tray = new NotifyIcon { Icon = warnIcon, Text = "Clipper", ContextMenuStrip = menu, Visible = true };
        tray.DoubleClick += (_, _) => Open(cfg.MarqueeUrl);

        obs = new ObsClient(cfg.Obs);
        obs.ConnectionChanged += up =>
        {
            valorantSourceShown = null;
            monitorChecked = false;
            if (!up) bufferActive = false;
            UpdateStatus();
        };
        obs.ReplaySaved += OnReplaySaved;
        games = new GameDetector(cfg);
        shipper = new ClipShipper(cfg, Notify);

        try
        {
            hotkey = new HotkeyWindow(cfg.Hotkey, SaveClip);
            Log.Info($"hotkey {cfg.Hotkey} registered");
        }
        catch (Exception ex)
        {
            Log.Error($"hotkey {cfg.Hotkey} unavailable", ex);
            Notify("Hotkey unavailable", $"{cfg.Hotkey}: {ex.Message}. Change \"hotkey\" in config.json.", true);
        }
        if (Config.LoadError is { } err) Notify("config.json is invalid", $"Running on defaults: {err}", true);

        shipper.Start();
        obs.Start();
        _ = Task.Run(WatchLoopAsync);
        UpdateStatus();
    }

    void SaveClip()
    {
        if (paused)
        {
            Notify("Clipping paused", "Resume from the tray menu to save clips.", false);
            return;
        }
        if (!obs.Connected)
        {
            Notify("Clip not saved", "OBS isn't connected.", true);
            return;
        }

        var save = new PendingSave(games.NameForSave(GameDetector.Snapshot()), DateTime.Now);
        lock (pendingLock) pendingSaves.Add(save);
        Log.Info($"hotkey: saving replay for {save.Game}");
        _ = Task.Run(async () =>
        {
            try
            {
                await obs.RequestAsync("SaveReplayBuffer");
            }
            catch (Exception ex)
            {
                lock (pendingLock) pendingSaves.Remove(save);
                Log.Error("SaveReplayBuffer failed", ex);
                Notify("Clip not saved", ex is ObsRequestException { Code: ObsClient.OutputNotRunning }
                    ? "The replay buffer isn't running (no game detected)."
                    : ex.Message, true);
            }
        });
    }

    void OnReplaySaved(string path)
    {
        PendingSave? save;
        lock (pendingLock)
        {
            pendingSaves.RemoveAll(p => DateTime.Now - p.When > TimeSpan.FromMinutes(2));
            save = pendingSaves.FirstOrDefault();
            if (save is not null) pendingSaves.RemoveAt(0);
        }
        // Saved from OBS itself rather than our hotkey: name it now.
        save ??= new PendingSave(games.NameForSave(GameDetector.Snapshot()), DateTime.Now);
        _ = shipper.AcceptAsync(path, save.Game, save.When);
    }

    async Task WatchLoopAsync()
    {
        while (!stop.IsCancellationRequested)
        {
            try { await TickAsync(); }
            catch (Exception ex) { Log.Error("watch tick", ex); }
            try { await Task.Delay(2000, stop.Token); }
            catch (OperationCanceledException) { break; }
        }
    }

    async Task TickAsync()
    {
        var snap = GameDetector.Snapshot();
        var state = games.Update(snap);
        activeGame = state.ActiveGame ?? "";
        if (!paused) EnsureObsRunning(snap);
        if (!obs.Connected)
        {
            UpdateStatus();
            return;
        }

        if (!monitorChecked) await EnsureDisplayCaptureAsync();
        if (valorantSourceShown != state.Valorant) await ShowValorantSourceAsync(state.Valorant);

        var status = await obs.RequestAsync("GetReplayBufferStatus");
        bufferActive = (bool?)status?["outputActive"] == true;

        bool want = !paused && state.ActiveGame is not null;
        if (want) lastGameActive = DateTime.Now;

        if (want && !bufferActive)
        {
            await IgnoreState(() => obs.RequestAsync("StartReplayBuffer"), ObsClient.OutputRunning);
            weStarted = bufferActive = true;
            Log.Info($"replay buffer started for {state.ActiveGame}");
        }
        else if (bufferActive && (paused || (weStarted && DateTime.Now - lastGameActive > TimeSpan.FromSeconds(cfg.StopGraceSeconds))))
        {
            await IgnoreState(() => obs.RequestAsync("StopReplayBuffer"), ObsClient.OutputNotRunning);
            weStarted = bufferActive = false;
            Log.Info(paused ? "replay buffer stopped (paused)" : "replay buffer stopped (no game running)");
        }
        UpdateStatus();
    }

    // Exactly one game capture is visible, so game audio (tracks 1 + 2) isn't doubled when both hook Valorant.
    async Task ShowValorantSourceAsync(bool valorant)
    {
        try
        {
            await SetItemEnabledAsync(ValorantSource, valorant);
            await SetItemEnabledAsync(AnyGameSource, !valorant);
            Log.Info($"capture source: {(valorant ? ValorantSource : AnyGameSource)}");
        }
        catch (ObsRequestException ex)
        {
            Log.Warn($"couldn't switch capture sources (is the Clipper scene collection active?): {ex.Message}");
        }
        valorantSourceShown = valorant;
    }

    // Display capture needs a monitor device id, which only OBS can enumerate: point it at the
    // primary monitor when it has none or its monitor is gone.
    async Task EnsureDisplayCaptureAsync()
    {
        try
        {
            var settings = await obs.RequestAsync("GetInputSettings", new JsonObject { ["inputName"] = DesktopSource });
            var current = (string?)settings?["inputSettings"]?["monitor_id"];
            var items = (await obs.RequestAsync("GetInputPropertiesListPropertyItems", new JsonObject
            {
                ["inputName"] = DesktopSource,
                ["propertyName"] = "monitor_id",
            }))?["propertyItems"]?.AsArray().OfType<JsonObject>().Where(i => (bool?)i["itemEnabled"] == true).ToList() ?? [];

            if (items.Count > 0 && !items.Any(i => (string?)i["itemValue"] == current))
            {
                var pick = items.FirstOrDefault(i => ((string?)i["itemName"])?.Contains("Primary") == true) ?? items[0];
                await obs.RequestAsync("SetInputSettings", new JsonObject
                {
                    ["inputName"] = DesktopSource,
                    ["inputSettings"] = new JsonObject { ["monitor_id"] = (string?)pick["itemValue"] },
                });
                Log.Info($"display capture -> {(string?)pick["itemName"]}");
            }
        }
        catch (ObsRequestException ex)
        {
            Log.Warn($"couldn't check display capture: {ex.Message}");
        }
        monitorChecked = true;
    }

    async Task SetItemEnabledAsync(string source, bool enabled)
    {
        var id = await obs.RequestAsync("GetSceneItemId", new JsonObject { ["sceneName"] = cfg.Obs.Scene, ["sourceName"] = source });
        await obs.RequestAsync("SetSceneItemEnabled", new JsonObject
        {
            ["sceneName"] = cfg.Obs.Scene,
            ["sceneItemId"] = (int)id!["sceneItemId"]!,
            ["sceneItemEnabled"] = enabled,
        });
    }

    static async Task IgnoreState(Func<Task> request, int alreadyCode)
    {
        try { await request(); }
        catch (ObsRequestException ex) when (ex.Code == alreadyCode) { }
    }

    void EnsureObsRunning(Dictionary<int, string> snap)
    {
        if (!cfg.Obs.Launch || snap.Values.Any(e => e.Equals("obs64.exe", StringComparison.OrdinalIgnoreCase))) return;
        if (DateTime.Now - lastObsLaunch < TimeSpan.FromSeconds(60)) return;
        lastObsLaunch = DateTime.Now;
        if (!File.Exists(cfg.Obs.Exe))
        {
            Log.Error($"OBS not found at {cfg.Obs.Exe}");
            return;
        }
        var psi = new ProcessStartInfo(cfg.Obs.Exe)
        {
            WorkingDirectory = Path.GetDirectoryName(cfg.Obs.Exe)!,   // OBS needs to start in its bin folder
            UseShellExecute = false,
        };
        foreach (var arg in new[] { "--profile", cfg.Obs.Profile, "--collection", cfg.Obs.Collection, "--minimize-to-tray", "--disable-shutdown-check" })
            psi.ArgumentList.Add(arg);
        Process.Start(psi)?.Dispose();
        Log.Info("launched OBS minimized to tray");
    }

    void TogglePause()
    {
        paused = !paused;
        pauseItem.Checked = paused;
        Log.Info(paused ? "clipping paused" : "clipping resumed");
        UpdateStatus();
    }

    void UpdateStatus() => ui.Post(_ =>
    {
        string text;
        Icon icon;
        if (paused) (text, icon) = ("Paused", warnIcon);
        else if (!obs.Connected) (text, icon) = ("Waiting for OBS…", warnIcon);
        else if (bufferActive) (text, icon) = ($"Recording replay{(activeGame.Length > 0 ? $" ({activeGame})" : "")}", recordingIcon);
        else (text, icon) = ("Idle, waiting for a game", idleIcon);

        statusItem.Text = text;
        var pending = shipper.PendingCount;
        queueItem.Text = pending > 0 ? $"Open queue folder ({pending} waiting)" : "Open queue folder";
        tray.Icon = icon;
        var tip = $"Clipper: {text}";
        tray.Text = tip.Length > 63 ? tip[..63] : tip;
    }, null);

    void Notify(string title, string text, bool error)
    {
        if (error) Log.Warn($"{title}: {text}");
        else Log.Info($"{title}: {text}");
        ui.Post(_ => tray.ShowBalloonTip(5000, title, text, error ? ToolTipIcon.Error : ToolTipIcon.Info), null);
    }

    static void Open(string target)
    {
        try
        {
            if (target == Paths.Queue) Directory.CreateDirectory(target);
            Process.Start(new ProcessStartInfo(target) { UseShellExecute = true })?.Dispose();
        }
        catch (Exception ex)
        {
            Log.Error($"couldn't open {target}", ex);
        }
    }

    void Quit()
    {
        stop.Cancel();
        if (weStarted && bufferActive && obs.Connected)
        {
            try { obs.RequestAsync("StopReplayBuffer").Wait(2000); }
            catch (Exception ex) { Log.Warn($"stop buffer on quit: {ex.Message}"); }
        }
        Log.Info("quit");
        tray.Visible = false;
        hotkey?.Dispose();
        obs.Dispose();
        ExitThread();
    }

    static Icon Dot(Color color)
    {
        using var bmp = new Bitmap(32, 32);
        using (var g = Graphics.FromImage(bmp))
        {
            g.SmoothingMode = SmoothingMode.AntiAlias;
            using var fill = new SolidBrush(color);
            using var ring = new Pen(Color.White, 3);
            g.FillEllipse(fill, 4, 4, 24, 24);
            g.DrawEllipse(ring, 4, 4, 24, 24);
        }
        return Icon.FromHandle(bmp.GetHicon());
    }
}
