-- powder-rs compatible implementation of Powder's `postprocess.utils` module.
local M = {}

-- Threshold checks on smoothed visual cues (signature kept for compatibility).
function M.checkHistory(frameIndex, historyLength, cueName, threshold, smoothedCues)
    if frameIndex < historyLength + 1 then return false end
    local s = smoothedCues[cueName]
    for i = 1, historyLength do
        if s[frameIndex - i] < threshold then return false end
    end
    return s[frameIndex] >= threshold
end

function M.checkFuture(frameIndex, futureLength, cueName, threshold, smoothedCues)
    local s = smoothedCues[cueName]
    if frameIndex + futureLength >= #s then return false end
    for i = 1, futureLength do
        if s[frameIndex + i] < threshold then return false end
    end
    return s[frameIndex] >= threshold
end

-- Convert internal events to the output format read by the engine.
function M.formatEventsDataTCT(events)
    local out = {}
    for _, e in pairs(events) do
        out[#out + 1] = { name = e.name, begin_frame_time = e.startFrameTime }
    end
    return out
end

function M.shallowCopy(t)
    local o = {}
    for k, v in pairs(t) do o[k] = v end
    return o
end

-- Build the debug timeline ("TCT") tables: numeric series go to TCT_values,
-- OCR strings go to TCT_descriptions (keyed ocrN::cropName like Powder does).
function M.TCTformatCuesData(cues)
    local result = { events = {}, TCT_values = {}, TCT_descriptions = {} }
    local n = 1
    for cueName, values in pairs(cues) do
        if cueName ~= "frameTimes" then
            if type(values) == "table" and (values.results ~= nil or values.confidences ~= nil) then
                local key = "ocr" .. n .. "::" .. cueName
                if values.confidences ~= nil then result.TCT_values[key] = values.confidences end
                if values.results ~= nil then result.TCT_descriptions[key] = values.results end
                n = n + 1
            else
                result.TCT_values[cueName] = values
            end
        end
    end
    return result
end

function M.findMostFrequent(t)
    local counts, best, bestCount = {}, nil, 0
    for _, v in pairs(t) do
        counts[v] = (counts[v] or 0) + 1
        if counts[v] >= bestCount then best, bestCount = v, counts[v] end
    end
    return best
end

return M
