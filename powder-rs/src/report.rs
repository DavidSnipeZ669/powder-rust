//! Self-contained HTML report: events + per-frame OCR text and cue values,
//! the equivalent of Powder's internal "TCT" debug timeline.

use crate::analyze::{fmt_ts, Analysis};

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

pub fn html(a: &Analysis) -> String {
    let mut ev = String::new();
    for e in &a.events {
        ev += &format!(
            "<tr class='{}'><td>{}</td><td>{}</td><td><code>{}</code></td><td>{}</td></tr>",
            if e.known { "" } else { "unknown" },
            e.timestamp,
            esc(&e.display_name),
            esc(&e.name),
            if e.known { "" } else { "not in events.json" }
        );
    }
    let text_keys: Vec<&String> = a.timeline_text.keys().collect();
    let val_keys: Vec<&String> = a.timeline_values.keys().filter(|k| !k.starts_with("ocr")).collect();
    let mut head = String::from("<th>#</th><th>time</th>");
    for k in &text_keys { head += &format!("<th>{}</th>", esc(k)); }
    for k in &val_keys { head += &format!("<th>{}</th>", esc(k)); }
    let mut rows = String::new();
    let ev_frames: Vec<usize> = a
        .events
        .iter()
        .map(|e| a.frame_times.iter().position(|t| (*t - e.time_sec).abs() < 1e-6).unwrap_or(usize::MAX))
        .collect();
    for (i, t) in a.frame_times.iter().enumerate() {
        let any_text = text_keys.iter().any(|k| a.timeline_text[*k].get(i).map(|s| !s.trim().is_empty()).unwrap_or(false));
        let any_val = val_keys.iter().any(|k| a.timeline_values[*k].get(i).map(|v| *v > 0.5).unwrap_or(false));
        let is_ev = ev_frames.contains(&i);
        let cls = if is_ev { "ev" } else if any_text || any_val { "hit" } else { "empty" };
        rows += &format!("<tr class='{cls}'><td>{i}</td><td>{}</td>", fmt_ts(*t));
        for k in &text_keys {
            rows += &format!("<td>{}</td>", esc(a.timeline_text[*k].get(i).map(|s| s.as_str()).unwrap_or("")));
        }
        for k in &val_keys {
            let v = a.timeline_values[*k].get(i).copied().unwrap_or(0.0);
            rows += &format!("<td style='background:rgba(80,200,120,{:.2})'>{:.2}</td>", v.clamp(0.0, 1.0) * 0.8, v);
        }
        rows += "</tr>";
    }
    let warn: String = a.warnings.iter().map(|w| format!("<li>{}</li>", esc(w))).collect();
    format!(
        r#"<!DOCTYPE html><html><head><meta charset="utf-8"><title>powder-rs: {game} report</title>
<style>
body{{font:14px system-ui,sans-serif;margin:24px;background:#111;color:#ddd}} h1{{font-size:20px}}
table{{border-collapse:collapse;margin:8px 0 24px}} td,th{{border:1px solid #333;padding:3px 8px;text-align:left;white-space:nowrap}}
th{{background:#222;position:sticky;top:0}} tr.ev td{{background:#3a2a00}} tr.unknown td{{color:#f77}}
tr.empty{{display:none}} body.all tr.empty{{display:table-row;color:#555}} code{{color:#9cf}} .warn{{color:#fb4}}
label{{cursor:pointer}}
</style></head><body>
<h1>{game} &mdash; {video}</h1>
<p>{w}x{h}, sampled at {fps} fps, {frames} frames, analysed in {secs:.1}s by {engine}</p>
<ul class="warn">{warn}</ul>
<h2>Events ({n})</h2><table><tr><th>time</th><th>event</th><th>id</th><th></th></tr>{ev}</table>
<h2>Timeline</h2><label><input type="checkbox" onchange="document.body.classList.toggle('all',this.checked)"> show frames with no detections</label>
<table><tr>{head}</tr>{rows}</table></body></html>"#,
        game = esc(&a.game),
        video = esc(&a.video),
        w = a.width,
        h = a.height,
        fps = a.fps,
        frames = a.frames,
        secs = a.seconds_taken,
        engine = a.engine,
        n = a.events.len(),
    )
}
