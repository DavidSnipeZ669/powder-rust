-- powder-rs compatible implementation of `postprocess.smoothing`.
local utils = require("postprocess.utils")
local M = {}

function M.median(values)
    local s = utils.shallowCopy(values)
    table.sort(s)
    local n = #s
    if n == 0 then return 0 end
    if n % 2 == 0 then return (s[n / 2] + s[n / 2 + 1]) / 2 end
    return s[math.ceil(n / 2)]
end

-- Exponentially weighted, centred moving average. Keeps short spikes when
-- smoothingPower is high; smoothingPower = 0 gives a plain box average.
function M.symmetricCenteredMovingAverage(signal, halfWindowSize, paddingValue, smoothingPower)
    paddingValue = paddingValue or M.median(signal)
    smoothingPower = smoothingPower or 3
    local out, n = {}, #signal
    for i = 1, n do
        local sum, total = 0, 0
        for j = i - halfWindowSize, i + halfWindowSize do
            local w = math.exp(-math.abs(i - j)) ^ smoothingPower
            local v = (j >= 1 and j <= n) and signal[j] or paddingValue
            sum = sum + v * w
            total = total + w
        end
        out[i] = sum / total
    end
    return out
end

function M.run(cuesConfig, cues, frameTimes)
    local out = {}
    for name, cfg in pairs(cuesConfig) do
        local signal = cues[name]
        if signal == nil then
            error("visual cue '" .. name .. "' is configured in visualCuesConfig but the model has no output with that name")
        end
        out[name] = M.symmetricCenteredMovingAverage(signal, cfg.halfWindowSmoothing, cfg.paddingValue, cfg.smoothingPower)
    end
    out.frameTimes = frameTimes
    return out
end

return M
