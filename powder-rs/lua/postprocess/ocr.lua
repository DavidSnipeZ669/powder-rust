-- powder-rs compatible implementation of `postprocess.ocr` (legacy Tesseract-style cues).
-- Results are plain strings ending in "\n" (Tesseract convention, kept for strict matches).
local M = {}

function M.splitString(s, sep)
    local out = {}
    for v in string.gmatch(s, "([^" .. sep .. "]+)") do out[#out + 1] = v end
    return out
end

function M.checkMatch(detected, wanted)
    return detected ~= nil and string.match(detected, wanted) ~= nil
end

function M.strictCheckMatch(detected, wanted)
    if not string.match(wanted, "\n") then wanted = wanted .. "\n" end
    return detected == wanted
end

function M.checkValues(detected, valuesToMatch)
    for _, wanted in pairs(valuesToMatch.values) do
        if valuesToMatch.strictMatch then
            if M.strictCheckMatch(detected, wanted) then return true end
        elseif M.checkMatch(detected, wanted) then
            return true
        end
    end
    return false
end

function M.checkFuture(cues, frameIndex, futureLength, cueName, valuesToMatch)
    if frameIndex + futureLength >= #cues.frameTimes then return false end
    for i = 1, futureLength do
        local d = cues[cueName].results[frameIndex + i]
        if d == nil or not M.checkValues(d, valuesToMatch) then return false end
    end
    return true
end

function M.checkHistory(cues, frameIndex, historyLength, cueName, valuesToMatch)
    if frameIndex < historyLength + 1 then return false end
    for i = 1, historyLength do
        local d = cues[cueName].results[frameIndex - i]
        if d == nil or not M.checkValues(d, valuesToMatch) then return false end
    end
    return true
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
