// powder-rs addition: "Add Custom Game Support" inside Powder's own interface. Nothing existing is changed;
// this only adds an entry to the left sidebar, a link under the game picker, and its own dialog.
(function () {
  const TOKEN = '__PW_TOKEN__'
  const H = { 'content-type': 'application/json', 'x-pw-token': TOKEN }
  const post = (p, b) => fetch(p, { method: 'POST', headers: H, body: JSON.stringify(b || {}) }).then((r) => r.json())
  const get = (p) => fetch(p, { headers: H }).then((r) => r.json())

  const css = `
  .pwrs-overlay{position:fixed;inset:0;background:rgba(10,10,14,.55);display:flex;align-items:flex-start;justify-content:center;z-index:99999;font-family:inherit;overflow-y:auto;padding:16px;box-sizing:border-box}
  .pwrs-modal{margin:auto;flex-shrink:0;background:#f4f4f4;color:#111;border-radius:24px;width:560px;max-width:92vw;padding:28px 30px;box-shadow:0 20px 60px rgba(0,0,0,.35)}
  .pwrs-modal h2{margin:0 0 6px;font-size:24px;font-weight:700}
  .pwrs-modal p.sub{margin:0 0 18px;color:#555;font-size:14px;line-height:1.4}
  /* Powder's own pop-ups (Sources' analyse dialog and others): scroll inside a small window
     instead of being cut off - centred while they fit, scrollable from the top when they don't */
  [class*="modal-Backdrop-style__backdrop"]{align-items:flex-start!important}
  [class*="modal-Backdrop-style__backdrop"]>*{margin:auto!important;flex-shrink:0!important;max-height:none!important}
  /* Sources' event filter list: scroll inside it instead of running off the window */
  [class*="CustomFilter-style__dropdown"]{max-height:min(60vh,calc(100vh - 300px))!important;overflow-y:auto!important}
  [class*="SelectInput-style__menu"]{max-height:60vh!important;overflow-y:auto!important}
  .pwrs-field{background:#fff;border-radius:18px;padding:14px 16px;margin-bottom:12px}
  .pwrs-field label{display:block;font-size:13px;font-weight:600;margin-bottom:8px}
  .pwrs-field input{width:100%;box-sizing:border-box;border:1px solid #ddd;border-radius:12px;padding:11px 12px;font:inherit;font-size:14px;background:#fafafa}
  .pwrs-row{display:flex;gap:10px;align-items:center}
  .pwrs-file{flex:1;font-size:13px;color:#444;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
  .pwrs-btn{border:0;border-radius:999px;padding:11px 20px;font:inherit;font-weight:600;font-size:14px;cursor:pointer;background:#e3e3e3;color:#111}
  .pwrs-btn.primary{background:#8b5cf6;color:#fff;padding:13px 26px;font-size:15px}
  .pwrs-btn:disabled{opacity:.5;cursor:default}
  .pwrs-actions{display:flex;justify-content:flex-end;gap:10px;margin-top:16px}
  .pwrs-log{background:#111;color:#cfcfcf;border-radius:14px;padding:10px 12px;font:12px/1.45 ui-monospace,Consolas,monospace;height:170px;overflow:auto;white-space:pre-wrap;margin-top:10px;display:none}
  .pwrs-status{font-size:14px;margin-top:10px;font-weight:600}
  .pwrs-link{display:block;width:max-content;margin:8px 0 0 4px;font-size:13px;color:#7c3aed;cursor:pointer;text-decoration:underline}
  .pwrs-nav{cursor:pointer}
  .pwrs-len{display:inline-flex;align-items:center;gap:4px;margin-left:10px;font:inherit;font-size:13px;opacity:.9}
  .pwrs-len input{width:64px;padding:3px 6px;border-radius:6px;border:1px solid #8886;background:transparent;color:inherit;font:inherit}
  .pwrs-modal.wide{width:1080px;max-width:96vw}
  .pwrs-ev{display:grid;grid-template-columns:150px 1fr 160px 78px 78px 86px 34px;gap:8px;align-items:center;margin-bottom:8px}
  .pwrs-ev input,.pwrs-ev select{width:100%;box-sizing:border-box;border:1px solid #ddd;border-radius:10px;padding:8px 10px;font:inherit;font-size:13px;background:#fafafa;color:inherit}
  .pwrs-ev .x{border:0;background:transparent;font-size:18px;cursor:pointer;color:#c33;border-radius:8px;height:34px}
  .pwrs-ev .x:hover{background:#c331}
  .pwrs-evhead{display:grid;grid-template-columns:150px 1fr 160px 78px 78px 86px 34px;gap:8px;font-size:12px;font-weight:700;opacity:.7;margin-bottom:6px}
  .pwrs-ev .imp{display:flex;align-items:center;gap:4px}.pwrs-ev .imp span{font-size:11px;opacity:.65;min-width:30px}
  .pwrs-tpl{position:fixed;inset:0;z-index:100000;background:rgba(10,10,14,.6);display:flex;align-items:flex-start;justify-content:center;overflow-y:auto;padding:16px;box-sizing:border-box}
  .pwrs-tpl .box{margin:auto;background:#2a2a2a;color:#f2f2f2;border-radius:20px;width:860px;max-width:94vw;padding:24px 26px}
  .pwrs-tpl input{width:100%;box-sizing:border-box;border:1px solid #3a3a3a;border-radius:12px;padding:10px 12px;font:inherit;background:#151515;color:inherit;margin:10px 0 14px}
  .pwrs-tpl .grid{display:grid;grid-template-columns:repeat(auto-fill,minmax(140px,1fr));gap:12px;max-height:60vh;overflow:auto}
  .pwrs-tpl .g{cursor:pointer;border-radius:12px;overflow:hidden;background:#1f1f1f;border:2px solid transparent}
  .pwrs-tpl .g:hover{border-color:#8b5cf6}
  .pwrs-tpl .g img{width:100%;aspect-ratio:3/4;object-fit:cover;display:block;background:#111}
  .pwrs-tpl .g div{font-size:12px;padding:6px 8px;white-space:nowrap;overflow:hidden;text-overflow:ellipsis}
  .pwrs-evlist{max-height:46vh;overflow:auto;padding-right:4px}
  .pwrs-chip{display:inline-block;border-radius:999px;padding:5px 11px;margin:0 6px 6px 0;font-size:12px;cursor:pointer;background:#e3e3e3}
  .pwrs-chip:hover{background:#d4d4d4}
  .pwrs-actions.split{justify-content:space-between}
  .pwrs-actions .grp{display:flex;gap:10px;flex-wrap:wrap}
  .theme__dark .pwrs-ev input,.theme__dark .pwrs-ev select{background:#151515;border-color:#3a3a3a;color:#eee}
  .theme__dark .pwrs-chip{background:#3a3a3a}
  .theme__dark .pwrs-chip:hover{background:#474747}
  .pwrs-modal select option{background:#fff;color:#111}
  .theme__dark .pwrs-modal select option{background:#1f1f1f;color:#eee}
  .theme__dark .pwrs-modal{background:#2a2a2a;color:#f2f2f2}
  .theme__dark .pwrs-modal p.sub{color:#bbb}
  .theme__dark .pwrs-field{background:#1f1f1f}
  .theme__dark .pwrs-field input{background:#151515;border-color:#3a3a3a;color:#eee}
  .theme__dark .pwrs-btn{background:#3a3a3a;color:#eee}
  .theme__dark .pwrs-btn.primary{background:#8b5cf6;color:#fff}`
  const st = document.createElement('style'); st.textContent = css; document.head.appendChild(st)

  function open(model) {
    if (document.querySelector('.pwrs-overlay')) return
    const o = document.createElement('div')
    o.className = 'pwrs-overlay'
    o.innerHTML = `<div class="pwrs-modal">
      <h2>${model ? 'Model Creation' : 'Add Custom Game Support'}</h2>
      <p class="sub">${model ? 'Teaches Rewind what each event <b>looks like</b>, so highlights are found even when the text on screen is missed. Every video you add saves pictures of its events and trains the game\'s model further (the model is saved encrypted as a .rwdmdl file).' : 'Type the game\'s name (or paste its Steam store link) and pick a gameplay video that shows some kills, wins or other highlights. Everything else is automatic.'}</p>
      ${model ? '<div style="border:1px solid #e0a03a;background:#e0a03a22;border-radius:12px;padding:10px 12px;margin:0 0 14px;font-size:13px;line-height:1.45"><b>⚠ This WILL use more storage.</b> Training needs thousands of pictures taken from your videos. They are kept so the model can keep improving, so the space used grows with the length of each video and keeps growing every time you add one (roughly 20–40 MB per 10 minutes of video). Stored in <a class="pwrs-link" id="pwrs-dsopen" style="display:inline;margin:0" title="Open the folder">Rewind\'s datasets folder</a> <span id="pwrs-dspath" style="opacity:.7;word-break:break-all"></span>. Delete a game\'s folder there to free the space (its model keeps working, it just can\'t keep improving from the old pictures).</div>' : ''}
      <div class="pwrs-field"><label>What to do</label><select id="pwrs-mode" style="width:100%;box-sizing:border-box;border:1px solid #8884;border-radius:12px;padding:11px 12px;font:inherit;font-size:14px;background:transparent;color:inherit"><option value="">${model ? 'New game (adds the game, then makes its model)' : 'Add a new game'}</option></select>
        <p class="sub" id="pwrs-modehint" style="margin:8px 0 0;display:none">The new video is scanned and combined with this game's earlier videos, so events your first video didn't show get detected too. Your own event edits are kept. The video also counts as analysed.</p></div>
      <div class="pwrs-field" id="pwrs-titlebox"><label>Game title or Steam link</label><input id="pwrs-title" placeholder="e.g. Delta Force  or  https://store.steampowered.com/app/2507950/Delta_Force/"></div>
      <div class="pwrs-field" id="pwrs-tplbox"><label>Events template (optional)</label><div class="pwrs-row"><div class="pwrs-file" id="pwrs-tplname">None: find the events from the video</div><button class="pwrs-btn" id="pwrs-tplpick">Choose game…</button></div>
        <p class="sub" style="margin:6px 0 0;font-size:12px">For another game in a series (e.g. a new Call of Duty): start from a game you already have. Its events, words, screen areas, clip times and importance are copied (for Powder games that use their own AI model instead of reading text, only the event names and settings).</p></div>
      <div class="pwrs-field"><label>Gameplay videos</label><div class="pwrs-row"><div class="pwrs-file" id="pwrs-file">No video selected</div><button class="pwrs-btn" id="pwrs-pick">Choose videos</button></div>
        <textarea id="pwrs-url" rows="2" placeholder="...and/or paste YouTube links, one per line (they're downloaded first)" style="margin-top:8px;width:100%;box-sizing:border-box;border:1px solid #8884;border-radius:12px;padding:10px 12px;font:inherit;font-size:14px;background:transparent;color:inherit;resize:vertical"></textarea>
        <p class="sub" style="margin:6px 0 0;font-size:12px">Several at once is fine. YouTube links download while the others are being worked on.</p>
        <label style="display:block;margin-top:8px;font-size:13px">Several videos: <select id="pwrs-par" style="border:1px solid #8884;border-radius:8px;padding:4px 6px;font:inherit;font-size:13px;background:transparent;color:inherit">
          <option value="">one at a time (lighter on your PC)</option><option value="1">scan two at once, then combine and train one by one (faster, heavier)</option></select></label></div>
      <div class="pwrs-status" id="pwrs-status"></div>
      <div class="pwrs-log" id="pwrs-log"></div>
      <div class="pwrs-after" id="pwrs-after" style="display:none;margin-top:12px;gap:10px;flex-wrap:wrap"></div>
      <a class="pwrs-link" id="pwrs-editlink" style="margin-top:12px">Edit the events of a game you already added…</a>
      <label style="display:flex;gap:8px;align-items:center;margin-top:10px;font-size:13px;opacity:.85;cursor:pointer"><input type="checkbox" id="pwrs-bottom" style="width:auto;margin:0"> Put games I add at the bottom of the game list (instead of A–Z among the others)</label>
      ${model ? '<a class="pwrs-link" id="pwrs-recheck" style="margin-top:12px;display:none">Re-check this game\'s pictures and retrain (no new video)</a><a class="pwrs-link" id="pwrs-restore" style="margin-top:6px;display:none">I sorted the pictures myself: undo the checks and lock my folders</a>' : ''}
      <div class="pwrs-actions"><button class="pwrs-btn" id="pwrs-close">Close</button><button class="pwrs-btn primary" id="pwrs-apply">Apply</button></div>
    </div>`
    // use Powder's own font (it is set on the app container, not on <body>)
    const ref = document.querySelector('.pwrs-nav') || document.querySelector('button')
    if (ref) o.style.fontFamily = getComputedStyle(ref).fontFamily
    document.body.appendChild(o)
    let files = []
    const $ = (id) => o.querySelector('#' + id)
    get('/__pw/x/edit-list').then((games) => {
      for (const g of games) { const op = document.createElement('option'); op.value = g.id; op.textContent = (model ? 'Train the "' + g.name + '" model with more videos' : 'Improve "' + g.name + '" with more videos'); $('pwrs-mode').appendChild(op) }
    })
    let template = ''
    $('pwrs-tplpick').onclick = async () => {
      const g = await pickGame('Use events from…')
      if (g) { template = g.id; $('pwrs-tplname').textContent = g.name } else { template = ''; $('pwrs-tplname').textContent = 'None: find the events from the video' }
    }
    $('pwrs-mode').onchange = () => { const add = !!$('pwrs-mode').value; $('pwrs-titlebox').style.display = add ? 'none' : ''; $('pwrs-tplbox').style.display = add ? 'none' : ''; $('pwrs-modehint').style.display = add ? '' : 'none'; if ($('pwrs-recheck')) $('pwrs-recheck').style.display = add ? '' : 'none'; if ($('pwrs-restore')) $('pwrs-restore').style.display = add ? '' : 'none' }
    if (model) $('pwrs-restore').onclick = async () => {
      const id = $('pwrs-mode').value
      if (!id) return
      const r = await post('/__pw/x/model-restore', { id })
      $('pwrs-status').textContent = r.ok ? r.message + '. Click "Re-check... and retrain" to train with them.' : 'Could not undo: ' + r.error
    }
    if (model) $('pwrs-recheck').onclick = async () => {
      const id = $('pwrs-mode').value
      if (!id) return
      $('pwrs-apply').disabled = true; $('pwrs-log').style.display = 'block'
      $('pwrs-status').textContent = 'Checking every picture again (this takes a while the first time)...'
      await post('/__pw/x/model-retrain', { id })
      const tick = async () => {
        const s = await get('/__pw/x/addgame')
        $('pwrs-log').textContent = s.log.join('\n'); $('pwrs-log').scrollTop = 1e9
        if (s.running) return setTimeout(tick, 800)
        $('pwrs-apply').disabled = false
        $('pwrs-status').textContent = s.error ? 'Could not retrain: ' + s.error : 'Done. Wrong pictures were moved to the right event; unclear ones are in the _review folder (not used).'
      }
      tick()
    }
    if (model) {
      get('/__pw/x/datasets').then((d) => { if (d && d.path) $('pwrs-dspath').textContent = '(' + d.path + ')' })
      $('pwrs-dsopen').onclick = () => post('/__pw/x/datasets-open')
    }
    get('/__pw/x/prefs').then((p) => { $('pwrs-bottom').checked = !!(p && p.customGamesAtBottom); $('pwrs-par').value = p && p.parallelScans ? '1' : '' })
    $('pwrs-par').onchange = () => post('/__pw/x/prefs', { key: 'parallelScans', value: $('pwrs-par').value === '1' })
    $('pwrs-bottom').onchange = () => post('/__pw/x/prefs', { key: 'customGamesAtBottom', value: $('pwrs-bottom').checked })
    $('pwrs-close').onclick = () => o.remove()
    $('pwrs-editlink').onclick = () => { o.remove(); openEditor() }
    o.addEventListener('mousedown', (e) => { if (e.target === o && !$('pwrs-apply').disabled) o.remove() })
    $('pwrs-pick').onclick = async () => {
      const r = await post('/__pw/x/pick-videos')
      const add = (r.paths || []).filter((p) => !files.includes(p))
      files = files.concat(add)
      $('pwrs-file').textContent = files.length ? (files.length === 1 ? files[0] : files.length + ' videos: ' + files.map((f) => f.split(/[\\/]/).pop()).join(', ')) : 'No video selected'
      $('pwrs-file').title = files.join('\n')
    }
    $('pwrs-apply').onclick = async () => {
      const title = $('pwrs-title').value.trim()
      const addTo = $('pwrs-mode').value
      if (!title && !addTo) { $('pwrs-status').textContent = 'Enter the game title or its Steam link.'; return }
      const links = $('pwrs-url').value.split(/\s+/).map((x) => x.trim()).filter((x) => /^https?:\/\//.test(x))
      const videos = files.concat(links)
      if (!videos.length) { $('pwrs-status').textContent = 'Choose gameplay videos or paste YouTube links.'; return }
      $('pwrs-apply').disabled = true; $('pwrs-pick').disabled = true
      $('pwrs-log').style.display = 'block'
      $('pwrs-status').textContent = videos.length > 1 ? `Working on ${videos.length} videos, one after another... this takes a while.` : 'Working... this takes a few minutes for a long video.'
      await post('/__pw/x/addgame', { title, videos, addTo, model: !!model, template: addTo ? '' : template, parallel: $('pwrs-par').value === '1' })
      const tick = async () => {
        const s = await get('/__pw/x/addgame')
        $('pwrs-log').textContent = s.log.join('\n'); $('pwrs-log').scrollTop = 1e9
        if (s.running) return setTimeout(tick, 800)
        $('pwrs-apply').disabled = false; $('pwrs-pick').disabled = false
        $('pwrs-status').textContent = s.error && !s.gameId ? 'Could not add the game: ' + s.error : (s.error ? 'Done, but ' + s.error : `Done${s.gameId ? ' (' + s.gameId + ')' : ''}. The game is now in the supported games list.`)
        showAfter(o, s.gameId, !!s.gameId)
      }
      tick()
    }
  }
  // after Add Game: edit the events, save the log for a bug report, share the game
  function showAfter(o, id, ok) {
    const box = o.querySelector('#pwrs-after')
    if (!box) return
    box.style.display = 'flex'
    box.innerHTML = ''
    const btn = (label, primary, fn) => { const b = document.createElement('button'); b.className = 'pwrs-btn' + (primary ? ' primary' : ''); b.textContent = label; b.onclick = fn; box.appendChild(b); return b }
    if (ok && id) btn('Review events', true, () => { o.remove(); openEditor(id) })
    btn('Export log', false, async () => {
      const r = await post('/__pw/x/export-log', { id })
      o.querySelector('#pwrs-status').textContent = r.ok ? 'Log saved to ' + r.path + '. Attach it when you report a problem.' : 'Could not save the log: ' + r.error
    })
    if (ok && id) btn('Share this game', false, async () => {
      const r = await post('/__pw/x/share-game', { id })
      o.querySelector('#pwrs-status').textContent = r.ok && r.uploaded ? 'Shared! It is waiting for a community reviewer to approve it (the page that opened shows its progress). Once approved, everyone using Rewind gets it automatically.' : r.ok ? 'Saved ' + r.zip + '. Drag it into the GitHub page that just opened and click Submit (needs a free GitHub account).' : 'Could not share: ' + (r.error || 'unknown error')
    })
    if (ok && id) o.querySelector('#pwrs-status').textContent += ' A video only shows some of a game\'s events: click Review events to remove wrong ones and add missing ones.'
  }

  // pick a game, the same list Sources shows (Powder's games and games added with Rewind)
  function pickGame(title) {
    return new Promise(async (resolve) => {
      const games = await get('/__pw/x/template-games')
      const t = document.createElement('div')
      t.className = 'pwrs-tpl'
      const ref = document.querySelector('.pwrs-nav') || document.querySelector('button')
      if (ref) t.style.fontFamily = getComputedStyle(ref).fontFamily
      t.innerHTML = `<div class="box"><h2 style="margin:0">${title}</h2><input placeholder="Search games" id="q"><div class="grid" id="gl"></div>
        <div class="pwrs-actions"><button class="pwrs-btn" id="c">Cancel</button></div></div>`
      document.body.appendChild(t)
      const done = (v) => { t.remove(); resolve(v) }
      const draw = () => {
        const q = t.querySelector('#q').value.toLowerCase()
        t.querySelector('#gl').innerHTML = games.filter((g) => (g.name || '').toLowerCase().includes(q)).map((g) =>
          `<div class="g" data-id="${esc(g.powderGameId)}" data-name="${esc(g.name)}"><img src="${esc(g.coverUrl || g.iconUrl || g.bannerUrl || '')}" onerror="this.style.visibility='hidden'"><div>${esc(g.name)}</div></div>`).join('')
        t.querySelectorAll('.g').forEach((c) => (c.onclick = () => done({ id: c.dataset.id, name: c.dataset.name })))
      }
      t.querySelector('#q').oninput = draw
      t.querySelector('#c').onclick = () => done(null)
      t.addEventListener('mousedown', (e) => { if (e.target === t) done(null) })
      draw(); t.querySelector('#q').focus()
    })
  }

  const COMMON = ['Kill', 'Headshot', 'Double Kill', 'Triple Kill', 'Quadra Kill', 'Penta Kill', 'Assist', 'Knock Down', 'Revive', 'Death', 'Victory', 'Defeat', 'Level Up', 'Killstreak', 'Objective']
  const esc = (t) => String(t).replace(/[&<>"]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' })[c])

  // event editor: one row per thing Rewind looks for on screen
  async function openEditor(id) {
    if (document.querySelector('.pwrs-overlay')) return
    const o = document.createElement('div')
    o.className = 'pwrs-overlay'
    const ref = document.querySelector('.pwrs-nav') || document.querySelector('button')
    if (ref) o.style.fontFamily = getComputedStyle(ref).fontFamily
    document.body.appendChild(o)
    const close = () => o.remove()
    if (!id) {
      const games = await get('/__pw/x/edit-list')
      o.innerHTML = `<div class="pwrs-modal"><h2>Edit a game's events</h2><p class="sub">Games added with Rewind (yours and community games).</p>
        <div class="pwrs-field">${games.length ? games.map((g) => `<div class="pwrs-chip" data-id="${esc(g.id)}">${esc(g.name)} <span style="opacity:.6">(${esc(g.id)})</span></div>`).join('') : 'No added games yet.'}</div>
        <div class="pwrs-actions"><button class="pwrs-btn" id="c">Close</button></div></div>`
      o.querySelector('#c').onclick = close
      o.querySelectorAll('[data-id]').forEach((c) => (c.onclick = () => { close(); openEditor(c.dataset.id) }))
      return
    }
    const g = await post('/__pw/x/edit-load', { id })
    if (g.error) { o.innerHTML = `<div class="pwrs-modal"><h2>Can't edit this game</h2><p class="sub">${esc(g.error)}</p><div class="pwrs-actions"><button class="pwrs-btn" id="c">Close</button></div></div>`; o.querySelector('#c').onclick = close; return }
    o.innerHTML = `<div class="pwrs-modal wide">
      <h2>Events: ${esc(g.name)}</h2>
      <p class="sub">Each row is something Rewind watches for on screen. A single video only shows some of a game's events, so: <b>remove</b> rows that aren't real events (player names, menu text), <b>fix</b> the words, and <b>add</b> events the video didn't show. Words are matched loosely, so small reading mistakes still count. Separate several words with commas.</p>
      <datalist id="pwrs-common">${COMMON.map((c) => `<option value="${c}">`).join('')}</datalist>
      <div class="pwrs-evhead"><div>Event</div><div>Text on screen</div><div>Where on screen</div><div title="Clip time before the event, in milliseconds">ms before</div><div title="Clip time after the event, in milliseconds">ms after</div><div title="How important the event is when choosing montage clips; the % is its share of all events">Importance</div><div></div></div>
      <div class="pwrs-evlist" id="rows"></div>
      <div style="display:flex;gap:10px;margin-top:6px"><button class="pwrs-btn" id="add">+ Add event</button><button class="pwrs-btn" id="tpl" title="Copy events from another game (e.g. an earlier game in the series)">Use a template…</button></div>
      <div id="sugg" style="margin-top:14px"></div>
      <div class="pwrs-status" id="st"></div>
      <div class="pwrs-actions split"><div class="grp"><button class="pwrs-btn" id="folder" title="events.json and game_postprocess.lua, for hand editing">Open game files</button><button class="pwrs-btn" id="reset" ${g.edited ? '' : 'disabled'}>Undo my edits</button></div>
        <div class="grp"><button class="pwrs-btn" id="cancel">Close</button><button class="pwrs-btn primary" id="save">Save</button></div></div>
    </div>`
    const $ = (s) => o.querySelector('#' + s)
    const areaName = (a) => (a === 'Anywhere' || a === g.fullScreen ? 'Anywhere on screen' : 'Area: ' + a)
    const areaOpts = (sel) => (g.areas.includes(sel) || !sel ? g.areas : g.areas.concat([sel])).map((a) => `<option value="${esc(a)}" ${a === sel ? 'selected' : ''}>${esc(areaName(a))}${g.areas.includes(a) ? '' : ' (from template)'}</option>`).join('')
    const keyOf = (d) => d.dataset.event || d.querySelector('.dn').value.trim().toLowerCase()
    // clip times and importance belong to the event: keep every row of one event the same
    const sync = (d, cls) => { const v = d.querySelector(cls).value; o.querySelectorAll('.pwrs-ev').forEach((x) => { if (x !== d && keyOf(x) === keyOf(d)) x.querySelector(cls).value = v }); shares() }
    const shares = () => {
      const seen = {}; o.querySelectorAll('.pwrs-ev').forEach((x) => { seen[keyOf(x)] = Math.max(0, parseFloat(x.querySelector('.im').value) || 0) })
      const total = Object.values(seen).reduce((a, b) => a + b, 0) || 1
      o.querySelectorAll('.pwrs-ev').forEach((x) => { x.querySelector('.imp span').textContent = Math.round((seen[keyOf(x)] || 0) * 100 / total) + '%' })
    }
    const addRow = (r) => {
      const d = document.createElement('div')
      d.className = 'pwrs-ev'
      d.dataset.event = r.event || ''
      if (r.areaCoords) d.dataset.coords = JSON.stringify(r.areaCoords)
      const same = [...o.querySelectorAll('.pwrs-ev')].find((x) => r.event && x.dataset.event === r.event)
      const val = (k, def) => (r[k] != null ? r[k] : same ? same.querySelector(k === 'msBefore' ? '.mb' : k === 'msAfter' ? '.ma' : '.im').value : def)
      d.innerHTML = `<input class="dn" list="pwrs-common" placeholder="e.g. Kill" value="${esc(r.displayName || '')}"><input class="w" placeholder="e.g. ELIMINATED, KILLED" value="${esc((r.words || []).join(', '))}"><select class="a">${areaOpts(r.area || g.fullScreen || 'Anywhere')}</select>` +
        `<input class="mb" type="number" min="0" step="100" value="${esc(val('msBefore', 1000))}"><input class="ma" type="number" min="0" step="100" value="${esc(val('msAfter', 1000))}"><div class="imp"><input class="im" type="number" min="0" step="1" value="${esc(val('importance', 10))}"><span></span></div><button class="x" title="Remove">✕</button>`
      d.querySelector('.x').onclick = () => { d.remove(); shares() }
      d.querySelector('.mb').oninput = () => sync(d, '.mb'); d.querySelector('.ma').oninput = () => sync(d, '.ma'); d.querySelector('.im').oninput = () => sync(d, '.im')
      d.querySelector('.a').onchange = () => { delete d.dataset.coords }
      // renaming to a different event starts a new one (keeps the old one's settings out of it)
      d.querySelector('.dn').addEventListener('change', () => { d.dataset.event = '' })
      $('rows').appendChild(d)
      shares()
      return d
    }
    g.rows.forEach(addRow)
    $('tpl').onclick = async () => {
      const pick = await pickGame('Use events from…')
      if (!pick) return
      const t = await post('/__pw/x/edit-template', { id: pick.id })
      if (t.error) { $('st').textContent = 'Could not load the template: ' + t.error; return }
      // events this game already has keep their rows (and get the template's clip times and
      // importance); new events are added
      let added = 0
      for (const r of t.rows) {
        const rows = [...o.querySelectorAll('.pwrs-ev')].filter((x) => x.dataset.event === r.event)
        if (rows.length) { rows.forEach((x) => { x.querySelector('.mb').value = r.msBefore; x.querySelector('.ma').value = r.msAfter; x.querySelector('.im').value = r.importance }); continue }
        addRow(r); added++
      }
      shares()
      $('st').textContent = `Template from ${t.name}: ${added} event row(s) added, clip times and importance copied. Check them, then Save.` + (t.official && !t.wordsKnown ? ' (Powder detects this game with its own AI model, so the words are just the event names: change them to the text your game shows.)' : t.official ? ' Words and screen areas come from Powder\'s own setup for that game: check the areas match where your game shows them.' : '')
    }
    $('add').onclick = () => { const d = addRow({}); d.querySelector('.dn').focus(); $('rows').scrollTop = 1e9 }
    if (g.suggestions && g.suggestions.length) {
      $('sugg').innerHTML = `<div style="font-size:13px;font-weight:700;margin-bottom:6px">Other text seen in your video (click to make it an event)</div>` +
        g.suggestions.map((s) => `<span class="pwrs-chip" data-w="${esc(s.word)}">${esc(s.word)} <span style="opacity:.6">${s.count}×</span></span>`).join('')
      $('sugg').querySelectorAll('[data-w]').forEach((c) => (c.onclick = () => { addRow({ displayName: '', words: [c.dataset.w] }).querySelector('.dn').focus(); c.remove() }))
    }
    $('cancel').onclick = close
    $('folder').onclick = () => post('/__pw/x/edit-open', { id })
    $('reset').onclick = async () => {
      if (!confirmed('reset')) return
      const r = await post('/__pw/x/edit-reset', { id })
      if (r.ok) { close(); openEditor(id) } else $('st').textContent = r.error
    }
    let armed = ''
    function confirmed(k) { if (armed === k) return true; armed = k; $('st').textContent = 'Click again to undo all your edits.'; return false }
    $('save').onclick = async () => {
      const rows = [...o.querySelectorAll('.pwrs-ev')].map((d) => {
        const r = { event: d.dataset.event, displayName: d.querySelector('.dn').value.trim(), words: d.querySelector('.w').value.split(',').map((w) => w.trim()).filter(Boolean), area: d.querySelector('.a').value,
          msBefore: Math.max(0, parseFloat(d.querySelector('.mb').value) || 0), msAfter: Math.max(0, parseFloat(d.querySelector('.ma').value) || 0), importance: Math.max(0, parseFloat(d.querySelector('.im').value) || 0) }
        if (d.dataset.coords) r.areaCoords = JSON.parse(d.dataset.coords)
        return r
      })
      $('save').disabled = true; $('st').textContent = 'Saving and reinstalling the game…'
      const r = await post('/__pw/x/edit-save', { id, rows })
      $('save').disabled = false
      $('st').textContent = r.ok ? `Saved: ${r.events} events. New analyses use them straight away.` : 'Could not save: ' + r.error
      if (r.ok) $('reset').disabled = false
    }
  }
  window.powderRsEditGame = openEditor

  window.powderRsAddGame = () => open(false)
  window.powderRsModelCreation = () => open(true)

  // 1) sidebar entry, cloned from Powder's own "Auto Montages" item so it looks native
  function addNav() {
    if (document.querySelector('.pwrs-nav')) return
    const btn = Array.from(document.querySelectorAll('button')).find((b) => /^Auto\s*Montages$/i.test((b.textContent || '').replace(/\s+/g, ' ').trim()))
    if (!btn) return
    const clone = btn.cloneNode(true)
    clone.classList.add('pwrs-nav')
    // never inherit the "selected" look of the original
    for (const c of Array.from(clone.classList)) if (/emphasis|active|selected/i.test(c)) clone.classList.remove(c)
    const label = Array.from(clone.querySelectorAll('span')).find((s) => /Montages/i.test(s.textContent || ''))
    if (label) label.innerHTML = 'Add<br> Game'
    const svg = clone.querySelector('svg')
    if (svg) svg.outerHTML = '<svg xmlns="http://www.w3.org/2000/svg" width="1em" height="1em" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="1" y="5" width="22" height="15" rx="5"/><path d="M7 10v5M4.5 12.5h5"/><circle cx="16" cy="11" r="1"/><circle cx="18.5" cy="14" r="1"/></svg>'
    clone.addEventListener('click', (e) => { e.preventDefault(); e.stopPropagation(); open() }, true)
    btn.parentElement.insertBefore(clone, btn.nextSibling)
    const mc = clone.cloneNode(true)
    mc.classList.add('pwrs-nav-model')
    const ml = Array.from(mc.querySelectorAll('span')).find((s) => /Add/i.test(s.textContent || ''))
    if (ml) ml.innerHTML = 'Model<br> Creation'
    const ms = mc.querySelector('svg')
    if (ms) ms.outerHTML = '<svg xmlns="http://www.w3.org/2000/svg" width="1em" height="1em" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="6" y="6" width="12" height="12" rx="2"/><path d="M9 2v4M15 2v4M9 18v4M15 18v4M2 9h4M2 15h4M18 9h4M18 15h4"/><circle cx="12" cy="12" r="2"/></svg>'
    mc.addEventListener('click', (e) => { e.preventDefault(); e.stopPropagation(); open(true) }, true)
    clone.parentElement.insertBefore(mc, clone.nextSibling)
  }
  // 2) "Game not listed?" link under the game picker in the analysis panel
  function addLink() {
    // the game picker is Powder's SelectInput whose placeholder reads "Select a supported game";
    // the link goes *below* the whole picker so the selected game's name keeps its space
    for (const el of document.querySelectorAll('[class*="SelectInput-style__select"]')) {
      const isGamePicker = /Select a supported game/i.test(el.textContent || '') || el.querySelector('img') ||
        /In-game events/i.test((el.parentElement && el.parentElement.textContent) || '')
      if (!isGamePicker) continue
      const next = el.nextElementSibling
      if (next && next.classList.contains('pwrs-link')) continue
      const a = document.createElement('div')
      a.className = 'pwrs-link'
      a.textContent = 'Game not listed? Add Custom Game Support'
      a.onclick = () => open(false)
      el.parentElement.insertBefore(a, el.nextSibling)
    }
    // remove links left inside pickers by older versions
    for (const l of document.querySelectorAll('[class*="SelectInput"] .pwrs-link')) l.remove()
  }
  // ---- Rewind branding: logo + name (Powder's interface files stay untouched) ----
  const LOGO = "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"112\" height=\"26\" fill=\"none\" viewBox=\"0 0 112 26\" aria-label=\"Rewind logo\"><path fill=\"url(#rwgrad)\" d=\"M0 0h23.258L26 2.742v19.5H12.39V26H2.743L0 23.258z\"/><path d=\"M15.742 17.367L23.242 26\" stroke=\"url(#rwgrad)\" stroke-width=\"8.4\" clip-path=\"url(#rwcw)\"/><path fill=\"#fff\" d=\"M23.258 0H0v23.258h9.648V19.5h13.61z\"/><path d=\"M13 17.367L20.5 26\" stroke=\"#fff\" stroke-width=\"8.4\" clip-path=\"url(#rwcw)\"/><path fill=\"#000\" d=\"M21.125 2.133H2.133v18.992h5.281V7.109s3.402.051 4.977-.05c1.574-.102 3.554-.559 3.554-.559v5.89s-2.437.508-4.062.56c-1.625.05-3.25.05-3.25.05v4.367h12.492z\"/><path d=\"M13 17.367L20.5 26\" stroke=\"#000\" stroke-width=\"4.2\" clip-path=\"url(#rwcb)\"/><path fill=\"currentColor\" transform=\"translate(-69.788 0)\" d=\"M107.788 19.611l.196.194h3.13l.195-.194v-4.667l.196-.194h1.369l.391.194 3.913 4.667.391.194h1.565l.196-.194v-2.528l-2.152-2.527v-.195c1.35-.564 2.543-1.75 2.543-3.889 0-2.722-1.976-4.278-4.891-4.278h-6.846l-.196.195zm3.521-10.305.196-.195h3.325c.783 0 1.37.195 1.37 1.361 0 1.167-.587 1.361-1.37 1.361h-3.325l-.196-.194z\"/><path fill=\"currentColor\" transform=\"translate(-46.019 0)\" d=\"M96.687 19.611l.195.194h9.977l.195-.194v-2.528l-.195-.194h-6.456l-.195-.195v-2.138l.195-.195h6.065l.195-.194v-2.528l-.195-.195h-6.065l-.195-.194V9.306l.195-.195h6.456l.195-.194V6.389l-.195-.195h-9.977l-.196.195z\"/><path fill=\"currentColor\" transform=\"translate(-3.325 0)\" d=\"M65.094 8.917l3.13 10.694.195.194h3.326l.195-.194 2.152-7.972h.196l2.152 7.972.195.194h3.326l.195-.194 3.13-10.694V6.389l-.195-.195h-2.739l-.196.195-1.76 7.972H78.2L76.048 6.39l-.195-.195h-3.326l-.195.195-2.152 7.972h-.196l-1.76-7.972-.196-.195h-2.739l-.195.195z\"/><path fill=\"currentColor\" transform=\"translate(80.695 0)\" d=\"M0 6.389V19.61l.196.194h3.13l.195-.194V6.389l-.195-.195H.196z\"/><path fill=\"currentColor\" transform=\"translate(84.95 0)\" d=\"M0 6.389V19.61l.196.194h3.13l.195-.194v-7.1h.2l4.3 7.1.391.194h3.4l.196-.194V6.389l-.196-.195h-3.13l-.195.195v7.1h-.2l-4.3-7.1-.391-.195H.196z\"/><path fill=\"currentColor\" transform=\"translate(13.764 0)\" d=\"M95.953 13c0-4.258-1.937-6.806-6.847-6.806h-4.89l-.196.195V19.61l.196.194h4.89c4.91 0 6.847-2.508 6.847-6.805m-3.521 0c0 3.13-.978 3.889-3.13 3.889h-1.565l-.196-.195V9.306l.196-.195h1.565c2.152 0 3.13.797 3.13 3.889\"/><defs><radialGradient id=\"rwgrad\" cx=\"0\" cy=\"0\" r=\"1\" gradientTransform=\"rotate(-33.611 51.39 -14.664)scale(75.3816 120.749)\" gradientUnits=\"userSpaceOnUse\"><stop offset=\"0.004\" stop-color=\"#BEFC5C\"></stop><stop offset=\"0.125\" stop-color=\"#FAFF00\"></stop><stop offset=\"0.217\" stop-color=\"#00FFE0\"></stop><stop offset=\"0.343\" stop-color=\"#FF00D6\"></stop><stop offset=\"0.525\" stop-color=\"#02ED53\"></stop></radialGradient><clipPath id=\"rwcw\"><rect x=\"0\" y=\"17.4\" width=\"40\" height=\"8.6\"/></clipPath><clipPath id=\"rwcb\"><rect x=\"0\" y=\"17.3\" width=\"40\" height=\"6.57\"/></clipPath></defs></svg>"
  function brandLogo() {
    const old = document.querySelector('svg[aria-label="Powder logo"]')
    if (!old) return
    const t = document.createElement('div')
    t.innerHTML = LOGO
    const svg = t.firstElementChild
    svg.setAttribute('class', old.getAttribute('class') || '') // same size/colour rules as Powder's logo
    old.replaceWith(svg)
  }
  const SKIP = new Set(['INPUT', 'TEXTAREA', 'SCRIPT', 'STYLE'])
  function brandText(root) {
    const w = document.createTreeWalker(root || document.body, NodeFilter.SHOW_TEXT)
    let n
    while ((n = w.nextNode())) {
      const t = n.nodeValue
      if (!t || t.indexOf('owder') < 0) continue
      if (/[\\/:]/.test(t)) continue // file paths stay as they are
      if (n.parentElement && SKIP.has(n.parentElement.tagName)) continue
      const r = t.replace(/\bPOWDER\b/g, 'REWIND').replace(/\bPowder\b/g, 'Rewind')
      if (r !== t) n.nodeValue = r
    }
    if (document.title !== 'Rewind') document.title = 'Rewind'
  }
  let brandQueued = false
  function brand() {
    if (brandQueued) return
    brandQueued = true
    requestAnimationFrame(() => { brandQueued = false; brandLogo(); brandText() })
  }

  // ---- exact montage length: a seconds box beside Powder's "maximum montage length" slider ----
  function addLengthBox() {
    for (const range of document.querySelectorAll('[class*="sliderSelection__slider"] input[type="range"]')) {
      const host = range.parentElement
      if (host.querySelector('.pwrs-len')) continue
      const box = document.createElement('label')
      box.className = 'pwrs-len'
      box.innerHTML = '<input type="number" min="10" step="1"> s'
      const inp = box.querySelector('input')
      const sync = () => { inp.max = range.max; if (document.activeElement !== inp) inp.value = range.value }
      sync()
      range.addEventListener('input', sync)
      new MutationObserver(sync).observe(range, { attributes: true, attributeFilter: ['max', 'value'] })
      const apply = () => {
        let v = Math.round(Number(inp.value))
        if (!isFinite(v)) return
        v = Math.max(10, Math.min(Number(range.max) || v, v))
        inp.value = v
        // drive Powder's own slider so its state (and the montage request) gets the exact value
        Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set.call(range, String(v))
        range.dispatchEvent(new Event('input', { bubbles: true }))
        setTimeout(() => range.dispatchEvent(new MouseEvent('mouseup', { bubbles: true })), 0)
      }
      inp.addEventListener('change', apply)
      inp.addEventListener('keydown', e => { if (e.key === 'Enter') { e.preventDefault(); apply() } })
      host.appendChild(box)
    }
  }

  const obs = new MutationObserver(() => { addNav(); addLink(); addLengthBox(); brand() })
  window.addEventListener('DOMContentLoaded', () => { obs.observe(document.body, { childList: true, subtree: true, characterData: true }); addNav(); addLink(); addLengthBox(); brand() })
})()
