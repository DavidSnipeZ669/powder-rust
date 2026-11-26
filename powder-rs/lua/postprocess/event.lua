-- powder-rs compatible implementation of `postprocess.event`.
local utils = require("postprocess.utils")
local M = {}

function M.event(name, startFrame, startFrameTime)
    return { name = name, startFrame = startFrame, startFrameTime = startFrameTime }
end

function M.initState(eventsSpecs)
    local s = {}
    for name, _ in pairs(eventsSpecs) do s[name] = {} end
    return s
end

-- Detections of the same event closer than `slack` frames are merged into one.
function M.mergeEvents(unmerged, eventsSpecs)
    local events = M.initState(eventsSpecs)
    table.sort(unmerged, function(a, b) return a.startFrame < b.startFrame end)
    for _, e in ipairs(unmerged) do
        local spec = eventsSpecs[e.name]
        if spec == nil then
            error("event '" .. tostring(e.name) .. "' was detected but is missing from eventsSpecs")
        end
        local list = events[e.name]
        local n = #list
        if n > 0 and list[n].startFrame >= e.startFrame - 1 - spec.slack then
            list[n].startFrame = e.startFrame
        else
            list[n + 1] = e
        end
    end
    return events
end

function M.flatten(events)
    local out = {}
    for _, list in pairs(events) do
        for _, e in pairs(list) do out[#out + 1] = e end
    end
    return out
end

function M.computeEvents(cues, eventsSpecs, functionsList)
    local found = {}
    for frameIndex = 1, #cues.frameTimes do
        for _, fn in pairs(functionsList) do
            local name, customIndex = fn(frameIndex)
            if name ~= nil then
                local idx = customIndex or frameIndex
                if idx < 1 then idx = 1 end
                if idx > #cues.frameTimes then idx = #cues.frameTimes end
                found[#found + 1] = M.event(name, idx, cues.frameTimes[idx])
            end
        end
    end
    return utils.formatEventsDataTCT(M.flatten(M.mergeEvents(found, eventsSpecs)))
end

return M
