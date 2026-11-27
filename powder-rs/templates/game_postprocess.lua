local smoothing = require("postprocess.smoothing")
local event = require("postprocess.event")
local utils = require("postprocess.utils")
local vc = require("postprocess.vc")
local paddle_ocr = require("postprocess.paddle_ocr")
local cues_data = require("postprocess.cues_data")

--[[  {{NAME}} ({{ID}})  -  created with powder-rs new-game

    How this works
    --------------
    1. Every frame (get_fps() per second) the engine cuts each crop below out of the screen
       and reads its text with OCR. Coordinates are fractions of the screen:
       { left, top, right, bottom }  (0,0 = top-left, 1,1 = bottom-right).
       Use `powder-rs studio` or `powder-rs calibrate` to find them.
    2. The DETECTORS table says which words in which crop mean which event.
    3. Every event name used here must also be in events.json (that's what Powder shows)
       and in EVENT_MERGE.

    This file works in powder-rs AND in the original Powder app (same API).
]]--

-- Analysis rate. powder-rs passes the rate you pick (1 ... 120+, or the recording's native
-- rate) in POWDER_RS_FPS; the original Powder app ignores that and uses DEFAULT_FPS.
-- Every timing below is in SECONDS, so detection behaves the same at any rate.
local DEFAULT_FPS = 4
local FPS = rawget(_G, "POWDER_RS_FPS") or DEFAULT_FPS
-- Rewind whole-word matching: "KILL" must be its own word ("KILLS", "MRSKILLER" don't count) and
-- longer phrases listed in a detector's `exclude` ("KILL CONFIRMED", "DOUBLE KILL") are removed
-- from the text first, so they can't trigger it.
local function rwTokens(s)
    local t = {}
    for w in string.gmatch(string.upper(s or ""), "[%w']+") do
        if not string.match(w, "^%d+$") then t[#t + 1] = w end
    end
    return t
end
local function rwSim(a, b)
    if a == b then return 100 end
    local la, lb = #a, #b
    if la == 0 or lb == 0 then return 0 end
    local prev = {}
    for j = 0, lb do prev[j] = j end
    for i = 1, la do
        local cur = { [0] = i }
        for j = 1, lb do
            local cost = (string.byte(a, i) == string.byte(b, j)) and 0 or 1
            cur[j] = math.min(prev[j] + 1, cur[j - 1] + 1, prev[j - 1] + cost)
        end
        prev = cur
    end
    return 100 * (1 - prev[lb] / math.max(la, lb))
end
local function rwFind(tokens, phrase, score)
    local p = rwTokens(phrase)
    if #p == 0 then return nil end
    for i = 1, #tokens - #p + 1 do
        local ok = true
        for j = 1, #p do
            local need = (#p[j] <= 5) and math.max(score, 85) or score
            if rwSim(tokens[i + j - 1], p[j]) < need then ok = false; break end
        end
        if ok then return i, #p end
    end
    return nil
end
local function rwMatch(text, d)
    if text == nil or text == "" then return false end
    local tokens = rwTokens(text)
    for _, ex in ipairs(d.exclude or {}) do
        local at, n = rwFind(tokens, ex, 90)
        local guard = 0
        while at and guard < 20 do
            for _ = 1, n do table.remove(tokens, at) end
            at, n = rwFind(tokens, ex, 90)
            guard = guard + 1
        end
    end
    for _, m in ipairs(d.match) do
        if rwFind(tokens, m, d.score) then return true end
    end
    return false
end
local function rwMatchFuture(cues, frameIndex, n, cropName, d)
    for i = 1, n do
        if not rwMatch(cues[cropName].results[frameIndex + i], d) then return false end
    end
    return true
end

local function frames(seconds) return math.max(1, math.floor(seconds * FPS + 0.5)) end

local get_fps = function()
    return DEFAULT_FPS
end

-- Visual cues from an optional model (model.json/model.onnx). Leave empty for OCR-only.
-- Example: ['visual_cues::kill_icon'] = { halfWindowSmoothing = 3 },
local visualCuesConfig = {}

local ocrConfig = {
    crops = {
        {
            cropName = "KillFeed",            -- e.g. "ELIMINATED <name>" banner
            debug = false,
            cropCoords = { 0.35, 0.62, 0.65, 0.70 },
            detectorDilateDiameter = 5,         -- joins letters into words (bigger = more joining)
            detectorMinimumArea = 300,          -- ignore tiny blobs (area in detector pixels, crop is scaled into 512x288)
            detectorMargin = 10,                -- padding around each text box
            recogniserStretchVertical = false,
            restrictedCharacters = ""           -- e.g. "0123456789" for score counters
        },
        {
            cropName = "GameResult",          -- "VICTORY" / "DEFEAT" end screen
            debug = false,
            cropCoords = { 0.30, 0.35, 0.70, 0.55 },
            detectorDilateDiameter = 7,
            detectorMinimumArea = 2000,
            detectorMargin = 20,
            recogniserStretchVertical = false,
            restrictedCharacters = ""
        }
    }
}

--[[ DETECTORS
    event   : event name (must exist in events.json and in eventsSpecs below)
    crop    : cropName from ocrConfig
    match   : list of words; fuzzy-matched, case/space-insensitive
    score   : 0-100 similarity needed (80 is a good start, lower = more forgiving)
    visible : seconds the text must stay on screen (filters OCR noise / flicker)
    offset  : shift the event time in seconds (negative = earlier, e.g. -0.5 to include the shot)
    priority: when several detectors on the same crop fire, the highest priority wins
]]--
local DETECTORS = {
    { event = "kill",     crop = "KillFeed",   match = { "ELIMINATED", "KILL" }, score = 80, visible = 0.4, offset = -0.5, priority = 1 },
    { event = "headshot", crop = "KillFeed",   match = { "HEADSHOT" },           score = 80, visible = 0.3, offset = -0.5, priority = 2 },
    { event = "victory",  crop = "GameResult", match = { "VICTORY" },            score = 75, visible = 0.5, offset = 0,    priority = 1 },
    { event = "defeat",   crop = "GameResult", match = { "DEFEAT" },             score = 75, visible = 0.5, offset = 0,    priority = 1 },
}

-- merge = seconds within which repeated detections of the same event count as one
local EVENT_MERGE = {
    kill     = 2.0,
    headshot = 2.0,
    victory  = 10.0,
    defeat   = 10.0,
}

local eventsSpecs = {}
for name, seconds in pairs(EVENT_MERGE) do
    eventsSpecs[name] = { name = name, slack = frames(seconds) }
end

local setEventsSpecs = function(cues)
    -- group detectors by crop so each crop yields at most one event per frame
    local byCrop, order = {}, {}
    for _, d in ipairs(DETECTORS) do
        if byCrop[d.crop] == nil then byCrop[d.crop] = {}; order[#order + 1] = d.crop end
        table.insert(byCrop[d.crop], d)
    end

    local functionsList = {}
    for _, cropName in ipairs(order) do
        local list = byCrop[cropName]
        table.insert(functionsList, function(frameIndex)
            if cues[cropName] == nil then return nil end
            local best, bestPrio = nil, -math.huge
            for _, d in ipairs(list) do
                local text = cues[cropName].results[frameIndex]
                local need = frames(d.visible or 0.4)
                if rwMatch(text, d)
                    and (need <= 1 or rwMatchFuture(cues, frameIndex, need - 1, cropName, d)) then
                    if (d.priority or 0) > bestPrio then best, bestPrio = d, d.priority or 0 end
                end
            end
            if best then return best.event, frameIndex + math.floor((best.offset or 0) * FPS + 0.5) end
            return nil
        end)
    end

    -- Add your own hand-written detectors here, e.g. combining OCR with visual cues:
    -- table.insert(functionsList, function(frameIndex)
    --     if vc.checkFuture(cues, frameIndex, frames(0.5), 'visual_cues::kill_icon', 0.8) then return 'kill' end
    -- end)

    return eventsSpecs, functionsList
end

-- DO NOT EDIT BELOW THIS LINE (standard Powder glue) ------------------------------------
local get_paddle_ocr_config = function()
    return ocrConfig
end

local computeEvents = function(modelOutputs, ocrOutput, frameTimes, paddleOcrOutput)
    local cues = {}
    if next(modelOutputs) ~= nil then
        for k, v in pairs(smoothing.run(visualCuesConfig, modelOutputs, frameTimes)) do cues[k] = v end
    else
        cues['frameTimes'] = frameTimes
    end
    if next(ocrOutput) ~= nil then
        for k, v in pairs(ocrOutput) do cues[k] = v end
    end
    if next(paddleOcrOutput) ~= nil then
        for k, v in pairs(paddleOcrOutput) do
            cues[k] = v
            v.confidences = {}
            for i, s in ipairs(v.results) do
                v.confidences[i] = (s == nil or s:match("^%s*$")) and 0 or 0.5
            end
        end
    end
    local result = utils.TCTformatCuesData(cues)
    local specs, functionsList = setEventsSpecs(cues)
    cues_data.cues = cues
    result.events = event.computeEvents(cues, specs, functionsList)
    return result
end

return {
    computeEvents = computeEvents,
    get_paddle_ocr_config = get_paddle_ocr_config,
    get_fps = get_fps,
    time_based = true,
}
