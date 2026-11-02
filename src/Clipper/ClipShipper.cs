namespace Clipper;

// Moves saved replays into the local queue under their final name, then copies them to
// <share>\Kept\<Game>\. A clip leaves the queue only after the copy on the share is verified.
sealed class ClipShipper(Config cfg, Action<string, string, bool> notify)
{
    static readonly HashSet<string> VideoExt = new(StringComparer.OrdinalIgnoreCase) { ".mp4", ".mkv", ".mov", ".webm" };
    const string OrphanGame = "Unsorted";

    readonly SemaphoreSlim wake = new(0, 1);
    volatile bool offline;

    public int PendingCount { get; private set; }

    public void Start() => _ = Task.Run(LoopAsync);

    public void Kick()
    {
        try { wake.Release(); }
        catch (SemaphoreFullException) { }
    }

    async Task LoopAsync()
    {
        Directory.CreateDirectory(Paths.Queue);
        RecoverOrphans();
        while (true)
        {
            try { await DrainAsync(); }
            catch (Exception ex) { Log.Error("queue pass failed", ex); }
            await wake.WaitAsync(TimeSpan.FromSeconds(Math.Max(10, cfg.RetrySeconds)));
        }
    }

    // Called when OBS reports a saved replay.
    public async Task AcceptAsync(string savedPath, string game, DateTime when)
    {
        try
        {
            await WaitUntilClosedAsync(savedPath);
            var dir = Path.Combine(Paths.Queue, game);
            Directory.CreateDirectory(dir);
            var dest = UniquePath(dir, $"{game} {when:yyyy-MM-dd HH-mm-ss}{Path.GetExtension(savedPath)}");
            File.Move(savedPath, dest);
            Log.Info($"queued {dest}");
            if (offline) notify("Clip saved locally", "Server unreachable; it uploads when the server is back.", true);
            Kick();
        }
        catch (Exception ex)
        {
            Log.Error($"couldn't queue {savedPath}", ex);
            notify("Clip not queued", $"{ex.Message}. The file is still at {savedPath}", true);
        }
    }

    async Task DrainAsync()
    {
        var files = Directory.EnumerateFiles(Paths.Queue, "*", SearchOption.AllDirectories)
            .Where(f => VideoExt.Contains(Path.GetExtension(f)))
            .OrderBy(File.GetLastWriteTimeUtc)
            .ToList();
        PendingCount = files.Count;
        if (files.Count == 0) return;

        if (string.IsNullOrWhiteSpace(cfg.Share) || !await Task.Run(() => Directory.Exists(cfg.Share)))
        {
            Log.Warn($"share {cfg.Share} unreachable; {files.Count} clip(s) queued");
            if (!offline) notify("Server unreachable", $"{files.Count} clip(s) kept in the local queue; retrying every {cfg.RetrySeconds} s.", true);
            offline = true;
            return;
        }
        if (offline) Log.Info("share reachable again");
        offline = false;

        foreach (var file in files)
        {
            var rel = Path.GetRelativePath(Paths.Queue, file);
            var game = Path.GetDirectoryName(rel) is { Length: > 0 } d ? d.Split(Path.DirectorySeparatorChar)[0] : cfg.FallbackGame;
            try
            {
                var dest = await Task.Run(() => Upload(file, game));
                PendingCount--;
                notify("Clip saved → marquee", $@"Kept\{game}\{Path.GetFileName(dest)}", false);
            }
            catch (Exception ex)
            {
                // Stays in the queue for the next pass.
                Log.Error($"upload of {file} failed", ex);
                notify("Clip upload failed", $"{Path.GetFileName(file)}: {ex.Message}. Retrying.", true);
            }
        }
    }

    string Upload(string file, string game)
    {
        var src = new FileInfo(file);
        long size = src.Length;
        var dir = Path.Combine(cfg.Share, "Kept", game);
        Directory.CreateDirectory(dir);

        var dest = Path.Combine(dir, src.Name);
        if (File.Exists(dest))
        {
            // A previous run copied it but died before deleting the local file.
            if (new FileInfo(dest).Length == size)
            {
                File.Delete(file);
                Log.Info($"{dest} already on the share; removed local copy");
                return dest;
            }
            dest = UniquePath(dir, src.Name);
        }

        // marquee only indexes video extensions, so it never sees a half-copied .part file.
        var part = dest + ".part";
        File.Copy(file, part, overwrite: true);
        var partSize = new FileInfo(part).Length;
        if (partSize != size)
        {
            TryDelete(part);
            throw new IOException($"size mismatch after copy ({partSize} != {size})");
        }
        File.Move(part, dest);
        var copied = new FileInfo(dest).Length;
        if (copied != size) throw new IOException($"size mismatch on share ({copied} != {size})");

        File.Delete(file);
        if (src.DirectoryName is { } srcDir && !srcDir.Equals(Paths.Queue, StringComparison.OrdinalIgnoreCase)
            && !Directory.EnumerateFileSystemEntries(srcDir).Any())
            TryDeleteDir(srcDir);
        Log.Info($"uploaded {dest} ({size:N0} bytes, verified)");
        return dest;
    }

    // Replays OBS saved while the companion wasn't around to name them.
    void RecoverOrphans()
    {
        if (!Directory.Exists(Paths.Recordings)) return;
        foreach (var file in Directory.EnumerateFiles(Paths.Recordings).Where(f => VideoExt.Contains(Path.GetExtension(f))).ToList())
        {
            var info = new FileInfo(file);
            if (DateTime.Now - info.LastWriteTime < TimeSpan.FromMinutes(2)) continue;
            try
            {
                var dir = Path.Combine(Paths.Queue, OrphanGame);
                Directory.CreateDirectory(dir);
                var dest = UniquePath(dir, $"{OrphanGame} {info.LastWriteTime:yyyy-MM-dd HH-mm-ss}{info.Extension}");
                File.Move(file, dest);
                Log.Info($"recovered orphaned replay {file} -> {dest}");
            }
            catch (Exception ex)
            {
                Log.Error($"couldn't recover {file}", ex);
            }
        }
    }

    static async Task WaitUntilClosedAsync(string path)
    {
        for (int i = 0; i < 120; i++)
        {
            try
            {
                using (new FileStream(path, FileMode.Open, FileAccess.Read, FileShare.None)) return;
            }
            catch (IOException) when (File.Exists(path))
            {
                await Task.Delay(500);
            }
        }
        throw new IOException("OBS was still writing the file after 60 s");
    }

    static string UniquePath(string dir, string fileName)
    {
        var path = Path.Combine(dir, fileName);
        var stem = Path.GetFileNameWithoutExtension(fileName);
        var ext = Path.GetExtension(fileName);
        for (int n = 2; File.Exists(path) || File.Exists(path + ".part"); n++)
            path = Path.Combine(dir, $"{stem} ({n}){ext}");
        return path;
    }

    static void TryDelete(string path)
    {
        try { File.Delete(path); } catch { }
    }

    static void TryDeleteDir(string path)
    {
        try { Directory.Delete(path); } catch { }
    }
}
