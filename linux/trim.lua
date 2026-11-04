-- clipper's trimmer for mpv. Loaded only by `clipper trim`, never by plain mpv.
--   I  set start        O  set end          C  clear marks
--   E  export (full quality)                D  export for Discord (under [clips] discord_mb)
-- Exports carry every audio track mixed into one (multi-track clips with a premixed track 1 keep it), as
-- H.264, land in <clips dir>/Trimmed Clips/ and are copied so you can paste them straight into Discord.
-- After an export the trimmer asks what to do with the original clip:
--   K  keep it (archive KEEP → Clips/Kept/<game>)   X  delete it (trash)   Esc  decide later
-- K and X close the trimmer; Esc lets you cut more trims from the same clip first.

local mp = require "mp"
local utils = require "mp.utils"
local input = require "mp.input"   -- mpv 0.38+: on-screen text box

local CLIP = os.getenv("CLIPPER_BIN") or (os.getenv("HOME") .. "/.local/bin/clipper")
local DISCORD_MB = mp.get_opt("clipper-discord_mb") or "10"
local a, b = nil, nil
local busy, asking = false, false
local prompt = mp.create_osd_overlay("ass-events")

-- palette in ASS colour order (&HBBGGRR&)
local ORANGE, FG, DIM, RED = "&H1A8CFF&", "&HBFCBD8&", "&H5A677A&", "&H4D48E5&"

local function fmt(t)
    if not t then return "--:--.-" end
    return string.format("%d:%04.1f", math.floor(t / 60), t % 60)
end

local function show(extra, secs)
    mp.osd_message(string.format("TRIM   IN %s   OUT %s%s\nI start · O end · C clear · E export · D Discord ≤%s MB",
        fmt(a), fmt(b), extra and ("   " .. extra) or "", DISCORD_MB), secs or 4)
end

local function now() return mp.get_property_number("time-pos") end

mp.add_forced_key_binding("i", "clipper-in", function()
    a = now(); if b and b <= a then b = nil end; show("start set")
end)
mp.add_forced_key_binding("o", "clipper-out", function()
    b = now(); if a and a >= b then a = nil end; show("end set")
end)
mp.add_forced_key_binding("c", "clipper-clear", function() a, b = nil, nil; show("cleared") end)

-- ── "what about the original?" prompt ──────────────────────────────────────
local function draw_prompt(saved, status)
    -- virtual canvas shaped like the window (at least 1080 tall, wide enough for the panel),
    -- so the whole prompt scales down together in a small or narrow trimmer window
    local ow, oh = mp.get_osd_size()
    if not ow or ow <= 0 or not oh or oh <= 0 then ow, oh = 1920, 1080 end
    local W, H = 1160, status and 370 or 330
    local ch = math.max(1080, math.ceil((W + 80) * oh / ow))
    local cw = math.floor(ch * ow / oh)
    prompt.res_x, prompt.res_y = cw, ch
    local cx, cy = math.floor(cw / 2), math.floor(ch / 2)
    local x0, y0 = cx - W / 2, cy - H / 2
    local function box(x, y, w, h, col, alpha)
        return string.format("{\\an7\\pos(%d,%d)\\bord0\\shad0\\1c%s\\1a&H%s&\\p1}m 0 0 l %d 0 %d %d 0 %d{\\p0}",
            x, y, col, alpha, w, w, h, h)
    end
    local F = "{\\fnJetBrainsMono Nerd Font\\bord0\\shad0}"
    local key = function(k, col) return "{\\1c" .. (col or ORANGE) .. "\\b1}" .. k .. "{\\b0\\1c" .. FG .. "}" end
    local lines = {
        "{\\fs40\\1c" .. ORANGE .. "\\b1}TRIM SAVED{\\b0\\fs28\\1c" .. DIM .. "}   Trimmed Clips / " .. saved,
        "{\\fs26\\1c" .. DIM .. "}copied · paste it into Discord",
        "{\\fs18} ",
        "{\\fs34\\1c" .. FG .. "\\b1}What about the original clip?{\\b0}",
        "{\\fs32\\1c" .. FG .. "}" .. key("K") .. "  keep it        " .. key("X", RED) .. "  delete it        "
            .. key("Esc") .. "  later",
    }
    if status then lines[#lines + 1] = "{\\fs26\\1c" .. DIM .. "}" .. status end
    prompt.data = table.concat({
        box(x0, y0, W, H, "&H0A0D12&", "18"),             -- bg #120d0a, ~90 % opaque
        box(x0, y0, W, 5, ORANGE, "00"),                   -- orange top edge, like the deck
        "{\\an5\\pos(" .. cx .. "," .. (cy + 3) .. ")}" .. F .. table.concat(lines, "\\N"),
    }, "\n")
    prompt:update()
end

local function close_prompt()
    asking = false
    prompt:remove()
    mp.remove_key_binding("clipper-orig-keep")
    mp.remove_key_binding("clipper-orig-delete")
    mp.remove_key_binding("clipper-orig-later")
end

local function decide(action, saved)
    if busy then return end
    busy = true
    draw_prompt(saved, action == "keep" and "keeping the original…" or "moving the original to the trash…")
    mp.command_native_async({
        name = "subprocess", playback_only = false, capture_stdout = true, capture_stderr = true,
        args = { CLIP, action, mp.get_property("path") },
    }, function(ok, res)
        busy = false
        if ok and res and res.status == 0 then
            draw_prompt(saved, action == "keep" and "original kept · closing" or "original deleted · closing")
            mp.add_timeout(1.2, function() mp.command("quit") end)
        else
            close_prompt()
            show("couldn't " .. action .. " the original: " .. ((res and res.stderr) or "?"):sub(-120), 8)
        end
    end)
end

local function ask(saved)
    asking = true
    draw_prompt(saved)
    mp.add_forced_key_binding("k", "clipper-orig-keep", function() decide("keep", saved) end)
    mp.add_forced_key_binding("x", "clipper-orig-delete", function() decide("discard", saved) end)
    mp.add_forced_key_binding("ESC", "clipper-orig-later", function() close_prompt(); show("original left in the archive", 4) end)
end

-- ── export ──────────────────────────────────────────────────────────────────
local function run_export(mode, s, e, title)
    busy = true
    show(mode == "discord" and "exporting for Discord…" or "exporting…", 30)
    mp.command_native_async({
        name = "subprocess", playback_only = false, capture_stdout = true, capture_stderr = true,
        args = { CLIP, "cut", mp.get_property("path"),
                 string.format("%.3f", s), string.format("%.3f", e), mode, title or "" },
    }, function(ok, res)
        busy = false
        if ok and res and res.status == 0 then
            local out = (res.stdout or ""):gsub("%s+$", "")
            local _, name = utils.split_path(out)
            mp.osd_message("", 0)
            ask(name)
        else
            show("export failed: " .. ((res and res.stderr) or "?"):sub(-120), 8)
        end
    end)
end

-- E / D: ask for a name first (Enter on an empty box = automatic name, Esc = cancel)
local function export(mode)
    if busy then show("already busy…"); return end
    if asking then close_prompt() end
    local s = a or 0
    local e = b or mp.get_property_number("duration")
    if not e or e - s < 0.5 then show("pick an end after the start"); return end
    local submitted = false
    input.get({
        prompt = (mode == "discord" and "Name this Discord trim" or "Name this trim")
                 .. " (Enter = auto, Esc = cancel): ",
        submit = function(text)
            submitted = true
            input.terminate()
            run_export(mode, s, e, text)
        end,
        closed = function()
            if not submitted then show("export cancelled", 3) end
        end,
    })
end

mp.add_forced_key_binding("e", "clipper-export", function() export("quality") end)
mp.add_forced_key_binding("d", "clipper-discord", function() export("discord") end)

mp.register_event("file-loaded", function() show(nil, 6) end)
