using System.Collections.Concurrent;
using System.Net.WebSockets;
using System.Security.Cryptography;
using System.Text;
using System.Text.Json.Nodes;

namespace Clipper;

sealed class ObsRequestException(string type, int code, string? comment)
    : Exception($"{type} failed ({code}{(comment is null ? "" : ": " + comment)})")
{
    public int Code { get; } = code;
}

// Minimal obs-websocket v5 client: reconnects forever, sends requests, raises Outputs events.
sealed class ObsClient(ObsSettings settings) : IDisposable
{
    // obs-websocket status codes we treat as "already in that state".
    public const int OutputRunning = 500, OutputNotRunning = 501;
    const int OutputsEvents = 1 << 6;

    readonly CancellationTokenSource cts = new();
    readonly ConcurrentDictionary<string, TaskCompletionSource<JsonObject>> pending = new();
    readonly SemaphoreSlim sendLock = new(1, 1);
    ClientWebSocket? socket;
    volatile bool connected;

    public event Action<bool>? ConnectionChanged;
    public event Action<string>? ReplaySaved;
    public bool Connected => connected;

    public void Start() => _ = Task.Run(RunAsync);

    async Task RunAsync()
    {
        string? lastError = null;
        while (!cts.IsCancellationRequested)
        {
            try
            {
                await SessionAsync(cts.Token);
                lastError = null;
            }
            catch (Exception ex)
            {
                if (cts.IsCancellationRequested) break;
                // OBS being closed is normal; log each distinct failure once instead of every 3 s.
                if (ex.Message != lastError) Log.Warn($"obs-websocket: {ex.Message}");
                lastError = ex.Message;
            }
            socket = null;
            SetConnected(false);
            foreach (var id in pending.Keys)
                if (pending.TryRemove(id, out var tcs)) tcs.TrySetException(new IOException("OBS disconnected"));
            try { await Task.Delay(3000, cts.Token); }
            catch (OperationCanceledException) { break; }
        }
    }

    async Task SessionAsync(CancellationToken ct)
    {
        using var ws = new ClientWebSocket();
        ws.Options.AddSubProtocol("obswebsocket.json");
        await ws.ConnectAsync(new Uri($"ws://{settings.Host}:{settings.Port}"), ct);

        var hello = await ReceiveAsync(ws, ct) ?? throw new IOException("closed before Hello");
        var identify = new JsonObject { ["rpcVersion"] = 1, ["eventSubscriptions"] = OutputsEvents };
        if (hello["d"]?["authentication"] is JsonObject auth)
            identify["authentication"] = AuthString(ResolvePassword(), (string)auth["salt"]!, (string)auth["challenge"]!);
        await SendAsync(ws, new JsonObject { ["op"] = 1, ["d"] = identify }, ct);

        var identified = await ReceiveAsync(ws, ct);
        if (identified is null || (int?)identified["op"] != 2)
            throw new IOException(ws.CloseStatus == (WebSocketCloseStatus)4009
                ? "authentication failed (check the obs-websocket password)"
                : $"identify rejected: {ws.CloseStatus} {ws.CloseStatusDescription}");

        socket = ws;
        SetConnected(true);
        Log.Info($"connected to obs-websocket on {settings.Host}:{settings.Port}");

        while (true)
        {
            var msg = await ReceiveAsync(ws, ct);
            if (msg is null)
            {
                Log.Info($"obs-websocket closed ({ws.CloseStatus} {ws.CloseStatusDescription})");
                return;
            }
            if (msg["d"] is not JsonObject d) continue;
            var op = (int?)msg["op"];
            if (op == 7 && (string?)d["requestId"] is { } id && pending.TryRemove(id, out var tcs))
                tcs.TrySetResult(d);
            else if (op == 5)
                OnEvent(d);
        }
    }

    void OnEvent(JsonObject d)
    {
        var data = d["eventData"];
        try
        {
            switch ((string?)d["eventType"])
            {
                case "ReplayBufferSaved":
                    var path = (string?)data?["savedReplayPath"];
                    Log.Info($"OBS saved replay: {path}");
                    if (!string.IsNullOrEmpty(path)) ReplaySaved?.Invoke(path);
                    break;
                case "ReplayBufferStateChanged":
                    Log.Info($"replay buffer: {(string?)data?["outputState"]}");
                    break;
            }
        }
        catch (Exception ex)
        {
            Log.Error("OBS event handler", ex);
        }
    }

    public async Task<JsonObject?> RequestAsync(string type, JsonObject? data = null, int timeoutMs = 5000)
    {
        var ws = socket;
        if (ws is null || !connected) throw new IOException("OBS isn't connected");

        var id = Guid.NewGuid().ToString("N");
        var tcs = new TaskCompletionSource<JsonObject>(TaskCreationOptions.RunContinuationsAsynchronously);
        pending[id] = tcs;
        var d = new JsonObject { ["requestType"] = type, ["requestId"] = id };
        if (data is not null) d["requestData"] = data;
        try
        {
            await SendAsync(ws, new JsonObject { ["op"] = 6, ["d"] = d }, cts.Token);
            var resp = await tcs.Task.WaitAsync(TimeSpan.FromMilliseconds(timeoutMs));
            var status = resp["requestStatus"];
            if ((bool?)status?["result"] != true)
                throw new ObsRequestException(type, (int?)status?["code"] ?? 0, (string?)status?["comment"]);
            return resp["responseData"] as JsonObject;
        }
        finally
        {
            pending.TryRemove(id, out _);
        }
    }

    void SetConnected(bool value)
    {
        if (connected == value) return;
        connected = value;
        ConnectionChanged?.Invoke(value);
    }

    async Task SendAsync(ClientWebSocket ws, JsonObject msg, CancellationToken ct)
    {
        var bytes = Encoding.UTF8.GetBytes(msg.ToJsonString());
        await sendLock.WaitAsync(ct);
        try { await ws.SendAsync(bytes, WebSocketMessageType.Text, true, ct); }
        finally { sendLock.Release(); }
    }

    static async Task<JsonNode?> ReceiveAsync(ClientWebSocket ws, CancellationToken ct)
    {
        var buf = new byte[16 * 1024];
        using var ms = new MemoryStream();
        while (true)
        {
            var r = await ws.ReceiveAsync(buf, ct);
            if (r.MessageType == WebSocketMessageType.Close) return null;
            ms.Write(buf, 0, r.Count);
            if (r.EndOfMessage) return JsonNode.Parse(ms.ToArray());
        }
    }

    string ResolvePassword()
    {
        if (!string.IsNullOrEmpty(settings.Password)) return settings.Password;
        var path = Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.ApplicationData),
            "obs-studio", "plugin_config", "obs-websocket", "config.json");
        return (string?)JsonNode.Parse(File.ReadAllText(path))?["server_password"] ?? "";
    }

    static string AuthString(string password, string salt, string challenge)
    {
        var secret = Convert.ToBase64String(SHA256.HashData(Encoding.UTF8.GetBytes(password + salt)));
        return Convert.ToBase64String(SHA256.HashData(Encoding.UTF8.GetBytes(secret + challenge)));
    }

    public void Dispose()
    {
        cts.Cancel();
        socket?.Abort();
    }
}
