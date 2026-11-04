namespace Clipper;

static class Program
{
    [STAThread]
    static int Main(string[] args)
    {
        Directory.CreateDirectory(Paths.Root);

        // Used by install.ps1: write config.json (keeping existing values) and exit.
        if (args.Length > 0 && args[0] == "--init-config")
        {
            var cfg = Config.Load();
            for (int i = 1; i + 1 < args.Length; i += 2)
            {
                switch (args[i])
                {
                    // empty = keep what's there (re-running install.ps1 without -Share)
                    case "--share" when args[i + 1].Length > 0: cfg.Share = args[i + 1]; break;
                    case "--marquee" when args[i + 1].Length > 0: cfg.MarqueeUrl = args[i + 1]; break;
                }
            }
            cfg.Save();
            return 0;
        }

        using var mutex = new Mutex(true, @"Local\ClipperCompanion", out bool first);
        if (!first) return 1;

        Application.SetHighDpiMode(HighDpiMode.PerMonitorV2);
        Application.EnableVisualStyles();
        Application.SetCompatibleTextRenderingDefault(false);
        Application.ThreadException += (_, e) => Log.Error("UI thread exception", e.Exception);
        AppDomain.CurrentDomain.UnhandledException += (_, e) => Log.Error("unhandled exception", e.ExceptionObject as Exception);
        TaskScheduler.UnobservedTaskException += (_, e) => { Log.Error("unobserved task exception", e.Exception); e.SetObserved(); };
        SynchronizationContext.SetSynchronizationContext(new WindowsFormsSynchronizationContext());

        Log.Info($"clipper {typeof(Program).Assembly.GetName().Version} starting");
        Application.Run(new TrayApp(Config.Load()));
        return 0;
    }
}
