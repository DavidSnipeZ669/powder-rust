-- powder-rs compatible implementation of `postprocess.vc` (visual cue window checks).
local M = {}

function M.checkFuture(cues, frameIndex, futureLength, cueName, threshold)
    if frameIndex + futureLength >= #cues.frameTimes then return false end
    for i = 1, futureLength do
        if cues[cueName][frameIndex + i] < threshold then return false end
    end
    return true
end

function M.checkHistory(cues, frameIndex, historyLength, cueName, threshold)
    if frameIndex < historyLength + 1 then return false end
    for i = 1, historyLength do
        if cues[cueName][frameIndex - i] < threshold then return false end
    end
    return true
end

return M
