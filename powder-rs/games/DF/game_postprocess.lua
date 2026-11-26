local smoothing = require("postprocess.smoothing")
local event = require("postprocess.event")
local utils = require("postprocess.utils")
local vc = require("postprocess.vc")
local paddle_ocr = require("postprocess.paddle_ocr")
local cues_data = require("postprocess.cues_data")

--[[  Delta Force (DF)  -  created with powder-rs new-game

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
local DEFAULT_FPS = 5
local FPS = rawget(_G, "POWDER_RS_FPS") or DEFAULT_FPS
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
            cropName = "KillMessage",         -- centre-bottom kill notification (first guess, calibrate!)
            debug = false,
            cropCoords = { 0.380, 0.650, 0.620, 0.700 },
            detectorDilateDiameter = 5,
            detectorMinimumArea = 300,
            detectorMargin = 8,
            recogniserStretchVertical = false,
            restrictedCharacters = ""
        },
        {
            cropName = "StreakMessage",       -- multi-kill banner (first guess, calibrate!)
            debug = false,
            cropCoords = { 0.300, 0.100, 0.700, 0.200 },
            detectorDilateDiameter = 5,
            detectorMinimumArea = 300,
            detectorMargin = 8,
            recogniserStretchVertical = false,
            restrictedCharacters = ""
        },
        {
            cropName = "GameResult",          -- end of match screen
            debug = false,
            cropCoords = { 0.350, 0.200, 0.650, 0.400 },
            detectorDilateDiameter = 7,
            detectorMinimumArea = 1500,
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
    -- KillMessage: specific phrases get higher priority than the generic "KILL"
    { event = "kill",            crop = "KillMessage",   match = { "ELIMINATED", "KILL", "TAKEDOWN" },       score = 80, visible = 0.4, offset = -0.5, priority = 1 },
    { event = "headshot",        crop = "KillMessage",   match = { "HEADSHOT", "HEAD SHOT" },                score = 80, visible = 0.4, offset = -0.5, priority = 2 },
    { event = "longshot",        crop = "KillMessage",   match = { "LONG SHOT" },                            score = 82, visible = 0.4, offset = -0.5, priority = 3 },
    { event = "preciselongshot", crop = "KillMessage",   match = { "PRECISE LONG SHOT" },                    score = 85, visible = 0.4, offset = -0.5, priority = 4 },
    { event = "meleeKill",       crop = "KillMessage",   match = { "MELEE KILL", "KNIFE KILL" },             score = 82, visible = 0.4, offset = -0.5, priority = 3 },
    { event = "gadgetDestroyed", crop = "KillMessage",   match = { "GADGET DESTROYED", "DEVICE DESTROYED" }, score = 80, visible = 0.4, offset = -0.3, priority = 2 },
    { event = "squadWipe",       crop = "KillMessage",   match = { "SQUAD WIPE", "SQUAD ELIMINATED" },       score = 80, visible = 0.4, offset = -0.3, priority = 5 },
    -- StreakMessage
    { event = "doubleKill",      crop = "StreakMessage", match = { "DOUBLE KILL" },                          score = 80, visible = 0.4, offset = -0.5, priority = 1 },
    { event = "tripleKill",      crop = "StreakMessage", match = { "TRIPLE KILL" },                          score = 80, visible = 0.4, offset = -0.5, priority = 2 },
    { event = "quadraKill",      crop = "StreakMessage", match = { "QUADRA KILL", "QUAD KILL" },             score = 80, visible = 0.4, offset = -0.5, priority = 3 },
    { event = "reaperMode",      crop = "StreakMessage", match = { "REAPER", "MULTI KILL" },                 score = 80, visible = 0.4, offset = -0.5, priority = 4 },
    -- GameResult
    { event = "victory",         crop = "GameResult",    match = { "VICTORY", "MISSION SUCCESS" },           score = 75, visible = 0.6, offset = 0,    priority = 1 },
    { event = "defeat",          crop = "GameResult",    match = { "DEFEAT", "MISSION FAILED" },             score = 75, visible = 0.6, offset = 0,    priority = 1 },
}

-- merge = seconds within which repeated detections of the same event count as one
local EVENT_MERGE = {
    kill = 1.6, headshot = 1.6, longshot = 1.6, preciselongshot = 1.6, meleeKill = 1.6,
    gadgetDestroyed = 1.6, squadWipe = 1.6,
    doubleKill = 3.2, tripleKill = 3.2, quadraKill = 3.2, reaperMode = 3.2,
    victory = 8.0, defeat = 8.0,
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
                if paddle_ocr.checkValues(text, d.match, d.score)
                    and (need <= 1 or paddle_ocr.checkFuture(frameIndex, need - 1, cropName, d.match, d.score)) then
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
