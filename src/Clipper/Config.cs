using System.Text.Json;

namespace Clipper;

static class Paths
{
    public static readonly string Root = Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "clipper");

    public static string Config => Path.Combine(Root, "config.json");
    public static string Log => Path.Combine(Root, "log.txt");
    // Clips waiting to be copied to the server, as queue\<Game>\<file>.mp4.
    public static string Queue => Path.Combine(Root, "queue");
    // Where the Clipper OBS profile writes replays before they are renamed into the queue.
    public static string Recordings => Path.Combine(Root, "recordings");
}

sealed class ObsSettings
{
    public string Host { get; set; } = "127.0.0.1";
    public int Port { get; set; } = 4455;
    // Empty: read server_password from OBS's own obs-websocket config.
    public string Password { get; set; } = "";
    public string Exe { get; set; } = @"C:\Program Files\obs-studio\bin\64bit\obs64.exe";
    // Start OBS (minimized to tray) when it isn't running.
    public bool Launch { get; set; } = true;
    public string Profile { get; set; } = "Clipper";
    public string Collection { get; set; } = "Clipper";
    public string Scene { get; set; } = "Clipper";
}

sealed class Config
{
    public const string ValorantExe = "VALORANT-Win64-Shipping.exe";

    public string Hotkey { get; set; } = "Alt+F9";
    // \\server\share; clips go to <share>\Kept\<Game>\.
    public string Share { get; set; } = "";
    public string MarqueeUrl { get; set; } = "http://192.168.50.67:8080";
    // false: only Valorant runs the replay buffer.
    public bool ClipOtherGames { get; set; } = true;
    // true: only exes in "games" count as games (besides Valorant).
    public bool OnlyListedGames { get; set; }
    // Folder for clips saved while no game is focused or running.
    public string FallbackGame { get; set; } = "Desktop";
    public int RetrySeconds { get; set; } = 60;
    // Keep the buffer running this long after the last game exits.
    public int StopGraceSeconds { get; set; } = 15;

    // exe -> folder name in Kept\. Exes not listed are named from their FileDescription.
    public Dictionary<string, string> Games { get; set; } = new()
    {
        [ValorantExe] = "Valorant",
        // Matches the folder the Arch desktop already uses on the share.
        ["GTA5.exe"] = "Grand-Theft-Auto-V",
        ["GTA5_Enhanced.exe"] = "Grand-Theft-Auto-V",
        ["cs2.exe"] = "Counter-Strike 2",
        ["FortniteClient-Win64-Shipping.exe"] = "Fortnite",
        ["r5apex.exe"] = "Apex Legends",
        ["r5apex_dx12.exe"] = "Apex Legends",
        ["League of Legends.exe"] = "League of Legends",
        ["RocketLeague.exe"] = "Rocket League",
        ["Overwatch.exe"] = "Overwatch 2",
        ["Minecraft.Windows.exe"] = "Minecraft",
        ["javaw.exe"] = "Minecraft",
    };

    // Never treated as games, even when fullscreen or focused on save.
    public List<string> Ignore { get; set; } =
    [
        "explorer.exe", "Discord.exe", "chrome.exe", "msedge.exe", "firefox.exe", "brave.exe",
        "opera.exe", "vivaldi.exe", "obs64.exe", "clipper.exe", "vlc.exe", "mpv.exe", "mpc-hc64.exe",
        "Spotify.exe", "steam.exe", "steamwebhelper.exe", "EpicGamesLauncher.exe", "RiotClientServices.exe",
        "Riot Client.exe", "ApplicationFrameHost.exe", "ShellExperienceHost.exe", "StartMenuExperienceHost.exe",
        "SearchHost.exe", "TextInputHost.exe", "LockApp.exe", "parsecd.exe", "Taskmgr.exe",
    ];

    public ObsSettings Obs { get; set; } = new();

    public static string? LoadError { get; private set; }

    static readonly JsonSerializerOptions Json = new()
    {
        WriteIndented = true,
        PropertyNamingPolicy = JsonNamingPolicy.CamelCase,
        ReadCommentHandling = JsonCommentHandling.Skip,
        AllowTrailingCommas = true,
    };

    public static Config Load()
    {
        Config cfg;
        if (!File.Exists(Paths.Config))
        {
            cfg = new Config();
            cfg.Save();
        }
        else
        {
            try
            {
                cfg = JsonSerializer.Deserialize<Config>(File.ReadAllText(Paths.Config), Json) ?? new Config();
            }
            catch (Exception ex)
            {
                // Run on defaults but leave the broken file alone for the user to fix.
                LoadError = ex.Message;
                Clipper.Log.Error($"config.json is invalid, using defaults: {ex.Message}");
                cfg = new Config();
            }
        }

        cfg.Games = cfg.Games.ToDictionary(kv => WithExe(kv.Key), kv => kv.Value, StringComparer.OrdinalIgnoreCase);
        cfg.Ignore = cfg.Ignore.Select(WithExe).ToList();
        return cfg;
    }

    public void Save()
    {
        Directory.CreateDirectory(Paths.Root);
        File.WriteAllText(Paths.Config, JsonSerializer.Serialize(this, Json));
    }

    public bool IsIgnored(string exe) => Ignore.Any(i => i.Equals(exe, StringComparison.OrdinalIgnoreCase));

    static string WithExe(string name) =>
        name.EndsWith(".exe", StringComparison.OrdinalIgnoreCase) ? name : name + ".exe";
}
