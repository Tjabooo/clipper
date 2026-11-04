using System.Diagnostics;
using System.Runtime.InteropServices;
using System.Text;
using System.Text.RegularExpressions;

namespace Clipper;

readonly record struct GameState(bool Valorant, string? ActiveGame);

// Decides whether a game is running and what to call it, using only window-manager calls and the
// system process list. It never injects into or reads the memory of any process. Exes listed in
// config "games" (Valorant included) are never even opened; other exes get a query-limited handle
// for their image path, and the name comes from the exe file on disk.
sealed class GameDetector(Config cfg)
{
    readonly Dictionary<int, (string Exe, string Name)> seen = new();   // fullscreen games still running
    readonly Dictionary<string, string> nameCache = new(StringComparer.OrdinalIgnoreCase);
    readonly object gate = new();
    readonly int ownPid = Environment.ProcessId;

    readonly record struct Foreground(int Pid, string Exe, bool Fullscreen);

    // pid -> "name.exe" from the system process snapshot (no process handles are opened).
    public static Dictionary<int, string> Snapshot()
    {
        var map = new Dictionary<int, string>();
        foreach (var p in Process.GetProcesses())
            using (p) map[p.Id] = p.ProcessName + ".exe";
        return map;
    }

    public GameState Update(Dictionary<int, string> snap)
    {
        lock (gate)
        {
            foreach (var (pid, game) in seen.ToList())
            {
                if (snap.TryGetValue(pid, out var exe) && exe.Equals(game.Exe, StringComparison.OrdinalIgnoreCase)) continue;
                seen.Remove(pid);
                Log.Info($"game exited: {game.Name}");
            }

            bool valorant = snap.Values.Any(IsValorant);
            if (cfg.ClipOtherGames && GetForeground(snap) is { Fullscreen: true } fg && IsGame(fg.Exe) && !seen.ContainsKey(fg.Pid))
            {
                var name = NameOf(fg.Pid, fg.Exe);
                seen[fg.Pid] = (fg.Exe, name);
                Log.Info($"fullscreen game: {fg.Exe} -> {name}");
            }

            string? active = valorant ? NameOf(0, Config.ValorantExe)
                : seen.Count > 0 ? seen.Values.Last().Name
                : null;
            return new GameState(valorant, active);
        }
    }

    // Folder name for a clip saved right now: the focused app, else the running game, else the fallback.
    public string NameForSave(Dictionary<int, string> snap)
    {
        lock (gate)
        {
            if (GetForeground(snap) is { } fg && IsGame(fg.Exe) && (cfg.ClipOtherGames || IsValorant(fg.Exe)))
                return NameOf(fg.Pid, fg.Exe);
            if (snap.Values.Any(IsValorant)) return NameOf(0, Config.ValorantExe);
            return seen.Count > 0 ? seen.Values.Last().Name : cfg.FallbackGame;
        }
    }

    bool IsGame(string exe) =>
        !cfg.IsIgnored(exe)
        && (!cfg.OnlyListedGames || IsValorant(exe) || cfg.Games.ContainsKey(exe));

    static bool IsValorant(string exe) => exe.Equals(Config.ValorantExe, StringComparison.OrdinalIgnoreCase);

    Foreground? GetForeground(Dictionary<int, string> snap)
    {
        var hwnd = Native.GetForegroundWindow();
        if (hwnd == IntPtr.Zero) return null;
        Native.GetWindowThreadProcessId(hwnd, out uint pid);
        if (pid == 0 || pid == ownPid || !snap.TryGetValue((int)pid, out var exe)) return null;

        var cls = new StringBuilder(256);
        Native.GetClassName(hwnd, cls, cls.Capacity);
        bool shell = cls.ToString() is "Progman" or "WorkerW" or "Shell_TrayWnd" or "Shell_SecondaryTrayWnd";
        return new Foreground((int)pid, exe, !shell && IsFullscreen(hwnd));
    }

    static bool IsFullscreen(IntPtr hwnd)
    {
        // A captioned window is a normal (maybe maximized) app window, even with an auto-hide taskbar.
        if (((uint)Native.GetWindowLong(hwnd, Native.GWL_STYLE) & Native.WS_CAPTION) == Native.WS_CAPTION) return false;
        if (!Native.GetWindowRect(hwnd, out var r)) return false;
        var info = new Native.MONITORINFO { cbSize = Marshal.SizeOf<Native.MONITORINFO>() };
        if (!Native.GetMonitorInfo(Native.MonitorFromWindow(hwnd, Native.MONITOR_DEFAULTTONEAREST), ref info)) return false;
        var m = info.rcMonitor;
        return r.Left <= m.Left && r.Top <= m.Top && r.Right >= m.Right && r.Bottom >= m.Bottom;
    }

    // Config mapping, else the exe's FileDescription, then ProductName, then the exe name.
    string NameOf(int pid, string exe)
    {
        if (cfg.Games.TryGetValue(exe, out var mapped)) return Sanitize(mapped, exe);
        if (IsValorant(exe)) return "Valorant";
        if (nameCache.TryGetValue(exe, out var cached)) return cached;

        string? name = null;
        var path = ImagePath(pid);
        if (path is not null)
        {
            try
            {
                var info = FileVersionInfo.GetVersionInfo(path);
                // Some exes (Windows 11 Notepad) just put their file name in FileDescription.
                bool useful = !string.IsNullOrWhiteSpace(info.FileDescription)
                    && !info.FileDescription.Trim().EndsWith(".exe", StringComparison.OrdinalIgnoreCase);
                name = useful ? info.FileDescription : info.ProductName;
            }
            catch (Exception ex)
            {
                Log.Warn($"no version info for {path}: {ex.Message}");
            }
        }
        name = Sanitize(name, exe);
        if (path is not null) nameCache[exe] = name;
        return name;
    }

    static string? ImagePath(int pid)
    {
        if (pid <= 0) return null;
        var handle = Native.OpenProcess(Native.PROCESS_QUERY_LIMITED_INFORMATION, false, pid);
        if (handle == IntPtr.Zero) return null;
        try
        {
            var sb = new StringBuilder(1024);
            int size = sb.Capacity;
            return Native.QueryFullProcessImageName(handle, 0, sb, ref size) ? sb.ToString() : null;
        }
        finally
        {
            Native.CloseHandle(handle);
        }
    }

    static string Sanitize(string? name, string exe)
    {
        var fallback = Path.GetFileNameWithoutExtension(exe);
        name = string.IsNullOrWhiteSpace(name) ? fallback : name;
        name = name.Replace("®", "").Replace("™", "").Replace("©", "");
        foreach (var c in Path.GetInvalidFileNameChars()) name = name.Replace(c, ' ');
        name = Regex.Replace(name, @"\s+", " ").Trim().Trim('.', ' ');
        if (name.Length > 60) name = name[..60].TrimEnd();
        return name.Length == 0 ? fallback : name;
    }
}
