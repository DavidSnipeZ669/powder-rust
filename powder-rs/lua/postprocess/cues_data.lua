-- Shared cue storage. The engine (and computeEvents) put the merged cues table here
-- so helper modules (paddle_ocr) can read it without it being passed around.
return { cues = {} }
