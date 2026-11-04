using System.ComponentModel;
using System.Runtime.InteropServices;

namespace Clipper;

// Message-only window that owns one global hotkey (RegisterHotKey, so no keyboard hook).
sealed class HotkeyWindow : NativeWindow, IDisposable
{
    const int Id = 1;
    readonly Action onPress;

    public HotkeyWindow(string spec, Action onPress)
    {
        this.onPress = onPress;
        var (mods, key) = Parse(spec);
        CreateHandle(new CreateParams { Parent = new IntPtr(-3) });   // HWND_MESSAGE
        if (!Native.RegisterHotKey(Handle, Id, mods | Native.MOD_NOREPEAT, (uint)key))
        {
            var err = Marshal.GetLastWin32Error();
            DestroyHandle();
            throw new Win32Exception(err, $"{spec} is already registered by another app");
        }
    }

    // "Alt+F9", "Ctrl+Shift+S", "Win+F10"...
    public static (uint Mods, Keys Key) Parse(string spec)
    {
        uint mods = 0;
        Keys? key = null;
        foreach (var raw in spec.Split('+', StringSplitOptions.RemoveEmptyEntries | StringSplitOptions.TrimEntries))
        {
            switch (raw.ToLowerInvariant())
            {
                case "alt": mods |= Native.MOD_ALT; break;
                case "ctrl" or "control": mods |= Native.MOD_CONTROL; break;
                case "shift": mods |= Native.MOD_SHIFT; break;
                case "win" or "windows": mods |= Native.MOD_WIN; break;
                default:
                    var name = raw.Length == 1 && char.IsDigit(raw[0]) ? "D" + raw : raw;
                    if (!Enum.TryParse<Keys>(name, ignoreCase: true, out var k)) throw new FormatException($"unknown key '{raw}' in hotkey '{spec}'");
                    key = k;
                    break;
            }
        }
        return (mods, key ?? throw new FormatException($"hotkey '{spec}' has no key"));
    }

    protected override void WndProc(ref Message m)
    {
        if (m.Msg == Native.WM_HOTKEY && m.WParam == Id) onPress();
        base.WndProc(ref m);
    }

    public void Dispose()
    {
        Native.UnregisterHotKey(Handle, Id);
        DestroyHandle();
    }
}
