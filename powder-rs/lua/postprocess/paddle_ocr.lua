-- powder-rs compatible implementation of `postprocess.paddle_ocr`.
-- Fuzzy matching of OCR text against wanted strings using Levenshtein similarity (0-100).
local cuesData = require("postprocess.cues_data")
local M = {}

local function levenshtein(s, t)
    local m, n = #s, #t
    if m == 0 then return n end
    if n == 0 then return m end
    local prev, cur = {}, {}
    for j = 0, n do prev[j] = j end
    for i = 1, m do
        cur[0] = i
        local si = s:byte(i)
        for j = 1, n do
            local cost = (si == t:byte(j)) and 0 or 1
            local a, b, c = prev[j] + 1, cur[j - 1] + 1, prev[j - 1] + cost
            cur[j] = (a < b) and ((a < c) and a or c) or ((b < c) and b or c)
        end
        prev, cur = cur, prev
    end
    return prev[n]
end

local function similarity(a, b)
    local maxLen = math.max(#a, #b)
    if maxLen == 0 then return 100 end
    return (1 - levenshtein(a, b) / maxLen) * 100
end

local function normalize(text)
    return (text:upper():gsub("%s+", ""))
end

M.levenshtein = levenshtein
M.similarity = similarity
M.normalize = normalize

-- Best similarity of `wanted` against any same-length window of `detected`.
function M.getBestSimilarityScore(detected, wanted)
    detected, wanted = normalize(detected), normalize(wanted)
    if #detected == 0 or #wanted == 0 then return 0 end
    local w = #wanted
    if #detected < w then return similarity(detected, wanted) end
    local best = 0
    for i = 1, #detected - w + 1 do
        local sc = similarity(detected:sub(i, i + w - 1), wanted)
        if sc > best then best = sc; if best >= 100 then break end end
    end
    return best
end

-- Results are memoised per (text, word list, score): at high frame rates the same OCR text
-- repeats for many consecutive frames, so this makes checkFuture/checkHistory almost free.
local memo = setmetatable({}, { __mode = "k" })
local memoCount = 0
function M.checkValues(detected, valuesToMatch, similarityScore)
    if detected == nil then return false end
    local byList = memo[valuesToMatch]
    if byList == nil then byList = {}; memo[valuesToMatch] = byList end
    local key = detected .. "\0" .. tostring(similarityScore)
    local hit = byList[key]
    if hit ~= nil then return hit end
    local res = false
    for _, wanted in pairs(valuesToMatch) do
        if M.getBestSimilarityScore(detected, wanted) >= similarityScore then res = true; break end
    end
    memoCount = memoCount + 1
    if memoCount > 200000 then memo = setmetatable({}, { __mode = "k" }); memoCount = 0 end
    byList[key] = res
    return res
end

function M.checkFuture(frameIndex, windowLength, ocrName, valuesToMatch, similarityScore)
    local cues = cuesData.cues
    if frameIndex + windowLength >= #cues.frameTimes then return false end
    for i = 1, windowLength do
        if not M.checkValues(cues[ocrName].results[frameIndex + i], valuesToMatch, similarityScore) then return false end
    end
    return true
end

function M.checkHistory(frameIndex, windowLength, ocrName, valuesToMatch, similarityScore)
    local cues = cuesData.cues
    if frameIndex < windowLength + 1 then return false end
    for i = 1, windowLength do
        if not M.checkValues(cues[ocrName].results[frameIndex - i], valuesToMatch, similarityScore) then return false end
    end
    return true
end

-- True when any wanted word appears at least twice (non-overlapping) in the text.
function M.checkMultipleOccurrences(input, valuesToMatch, similarityScore)
    if not input or input == "" then return false end
    local s = normalize(input)
    for _, word in ipairs(valuesToMatch) do
        local w = normalize(word)
        local count, i = 0, 1
        while i <= #s - #w + 1 do
            if similarity(s:sub(i, i + #w - 1), w) >= similarityScore then
                count = count + 1
                if count >= 2 then return true end
                i = i + #w
            else
                i = i + 1
            end
        end
    end
    return false
end

return M
