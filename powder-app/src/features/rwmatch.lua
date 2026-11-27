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
