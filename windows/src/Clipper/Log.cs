namespace Clipper;

static class Log
{
    const long MaxBytes = 5 * 1024 * 1024;
    static readonly object Gate = new();

    public static void Info(string msg) => Write("INFO ", msg);
    public static void Warn(string msg) => Write("WARN ", msg);
    public static void Error(string msg, Exception? ex = null) =>
        Write("ERROR", ex is null ? msg : $"{msg}: {ex}");

    static void Write(string level, string msg)
    {
        lock (Gate)
        {
            try
            {
                Directory.CreateDirectory(Paths.Root);
                var file = new FileInfo(Paths.Log);
                if (file.Exists && file.Length > MaxBytes)
                    File.Move(Paths.Log, Path.Combine(Paths.Root, "log.1.txt"), overwrite: true);
                File.AppendAllText(Paths.Log, $"{DateTime.Now:yyyy-MM-dd HH:mm:ss.fff} {level} {msg}{Environment.NewLine}");
            }
            catch
            {
                // Logging must never take the app down.
            }
        }
    }
}
