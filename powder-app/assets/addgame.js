// powder-rs addition: "Add Custom Game Support" inside Powder's own interface. Nothing existing is changed;
// this only adds an entry to the left sidebar, a link under the game picker, and its own dialog.
(function () {
  const TOKEN = '__PW_TOKEN__'
  const H = { 'content-type': 'application/json', 'x-pw-token': TOKEN }
  const post = (p, b) => fetch(p, { method: 'POST', headers: H, body: JSON.stringify(b || {}) }).then((r) => r.json())
  const get = (p) => fetch(p, { headers: H }).then((r) => r.json())

  const css = `
  .pwrs-overlay{position:fixed;inset:0;background:rgba(10,10,14,.55);display:flex;align-items:center;justify-content:center;z-index:99999;font-family:inherit}
  .pwrs-modal{background:#f4f4f4;color:#111;border-radius:24px;width:560px;max-width:92vw;padding:28px 30px;box-shadow:0 20px 60px rgba(0,0,0,.35)}
  .pwrs-modal h2{margin:0 0 6px;font-size:24px;font-weight:700}
  .pwrs-modal p.sub{margin:0 0 18px;color:#555;font-size:14px;line-height:1.4}
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
  .pwrs-modal.wide{width:820px}
  .pwrs-ev{display:grid;grid-template-columns:170px 1fr 180px 34px;gap:8px;align-items:center;margin-bottom:8px}
  .pwrs-ev input,.pwrs-ev select{width:100%;box-sizing:border-box;border:1px solid #ddd;border-radius:10px;padding:8px 10px;font:inherit;font-size:13px;background:#fafafa;color:inherit}
  .pwrs-ev .x{border:0;background:transparent;font-size:18px;cursor:pointer;color:#c33;border-radius:8px;height:34px}
  .pwrs-ev .x:hover{background:#c331}
  .pwrs-evhead{display:grid;grid-template-columns:170px 1fr 180px 34px;gap:8px;font-size:12px;font-weight:700;opacity:.7;margin-bottom:6px}
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

  function open() {
    if (document.querySelector('.pwrs-overlay')) return
    const o = document.createElement('div')
    o.className = 'pwrs-overlay'
    o.innerHTML = `<div class="pwrs-modal">
      <h2>Add Custom Game Support</h2>
      <p class="sub">Type the game's name (or paste its Steam store link) and pick a gameplay video that shows some kills, wins or other highlights. Everything else is automatic.</p>
      <div class="pwrs-field"><label>What to do</label><select id="pwrs-mode" style="width:100%;box-sizing:border-box;border:1px solid #8884;border-radius:12px;padding:11px 12px;font:inherit;font-size:14px;background:transparent;color:inherit"><option value="">Add a new game</option></select>
        <p class="sub" id="pwrs-modehint" style="margin:8px 0 0;display:none">The new video is scanned and combined with this game's earlier videos, so events your first video didn't show get detected too. Your own event edits are kept. The video also counts as analysed.</p></div>
      <div class="pwrs-field" id="pwrs-titlebox"><label>Game title or Steam link</label><input id="pwrs-title" placeholder="e.g. Delta Force  or  https://store.steampowered.com/app/2507950/Delta_Force/"></div>
      <div class="pwrs-field"><label>Gameplay video</label><div class="pwrs-row"><div class="pwrs-file" id="pwrs-file">No video selected</div><button class="pwrs-btn" id="pwrs-pick">Choose video</button></div></div>
      <div class="pwrs-status" id="pwrs-status"></div>
      <div class="pwrs-log" id="pwrs-log"></div>
      <div class="pwrs-after" id="pwrs-after" style="display:none;margin-top:12px;gap:10px;flex-wrap:wrap"></div>
      <a class="pwrs-link" id="pwrs-editlink" style="margin-top:12px">Edit the events of a game you already added…</a>
      <div class="pwrs-actions"><button class="pwrs-btn" id="pwrs-close">Close</button><button class="pwrs-btn primary" id="pwrs-apply">Apply</button></div>
    </div>`
    // use Powder's own font (it is set on the app container, not on <body>)
    const ref = document.querySelector('.pwrs-nav') || document.querySelector('button')
    if (ref) o.style.fontFamily = getComputedStyle(ref).fontFamily
    document.body.appendChild(o)
    let video = ''
    const $ = (id) => o.querySelector('#' + id)
    get('/__pw/x/edit-list').then((games) => {
      for (const g of games) { const op = document.createElement('option'); op.value = g.id; op.textContent = 'Improve "' + g.name + '" with another video'; $('pwrs-mode').appendChild(op) }
    })
    $('pwrs-mode').onchange = () => { const add = !!$('pwrs-mode').value; $('pwrs-titlebox').style.display = add ? 'none' : ''; $('pwrs-modehint').style.display = add ? '' : 'none' }
    $('pwrs-close').onclick = () => o.remove()
    $('pwrs-editlink').onclick = () => { o.remove(); openEditor() }
    o.addEventListener('mousedown', (e) => { if (e.target === o && !$('pwrs-apply').disabled) o.remove() })
    $('pwrs-pick').onclick = async () => {
      const r = await post('/__pw/x/pick-video')
      if (r.path) { video = r.path; $('pwrs-file').textContent = r.path }
    }
    $('pwrs-apply').onclick = async () => {
      const title = $('pwrs-title').value.trim()
      const addTo = $('pwrs-mode').value
      if (!title && !addTo) { $('pwrs-status').textContent = 'Enter the game title or its Steam link.'; return }
      if (!video) { $('pwrs-status').textContent = 'Choose a gameplay video.'; return }
      $('pwrs-apply').disabled = true; $('pwrs-pick').disabled = true
      $('pwrs-log').style.display = 'block'
      $('pwrs-status').textContent = 'Working... this takes a few minutes for a long video.'
      await post('/__pw/x/addgame', { title, video, addTo })
      const tick = async () => {
        const s = await get('/__pw/x/addgame')
        $('pwrs-log').textContent = s.log.join('\n'); $('pwrs-log').scrollTop = 1e9
        if (s.running) return setTimeout(tick, 800)
        $('pwrs-apply').disabled = false; $('pwrs-pick').disabled = false
        $('pwrs-status').textContent = s.error ? 'Could not add the game: ' + s.error : `Done${s.gameId ? ' (' + s.gameId + ')' : ''}. The game is now in the supported games list.`
        showAfter(o, s.gameId, !s.error)
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
      o.querySelector('#pwrs-status').textContent = r.ok ? 'Saved ' + r.zip + '. Drag it into the GitHub page that just opened and click Submit (needs a free GitHub account).' : 'Could not share: ' + (r.error || 'unknown error')
    })
    if (ok && id) o.querySelector('#pwrs-status').textContent += ' A video only shows some of a game\'s events: click Review events to remove wrong ones and add missing ones.'
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
      <div class="pwrs-evhead"><div>Event</div><div>Text on screen</div><div>Where on screen</div><div></div></div>
      <div class="pwrs-evlist" id="rows"></div>
      <button class="pwrs-btn" id="add" style="margin-top:6px">+ Add event</button>
      <div id="sugg" style="margin-top:14px"></div>
      <div class="pwrs-status" id="st"></div>
      <div class="pwrs-actions split"><div class="grp"><button class="pwrs-btn" id="folder" title="events.json and game_postprocess.lua, for hand editing">Open game files</button><button class="pwrs-btn" id="reset" ${g.edited ? '' : 'disabled'}>Undo my edits</button></div>
        <div class="grp"><button class="pwrs-btn" id="cancel">Close</button><button class="pwrs-btn primary" id="save">Save</button></div></div>
    </div>`
    const $ = (s) => o.querySelector('#' + s)
    const areaName = (a) => (a === 'Anywhere' || a === g.fullScreen ? 'Anywhere on screen' : 'Area: ' + a)
    const areaOpts = (sel) => g.areas.map((a) => `<option value="${esc(a)}" ${a === sel ? 'selected' : ''}>${esc(areaName(a))}</option>`).join('')
    const addRow = (r) => {
      const d = document.createElement('div')
      d.className = 'pwrs-ev'
      d.dataset.event = r.event || ''
      d.innerHTML = `<input class="dn" list="pwrs-common" placeholder="e.g. Kill" value="${esc(r.displayName || '')}"><input class="w" placeholder="e.g. ELIMINATED, KILLED" value="${esc((r.words || []).join(', '))}"><select class="a">${areaOpts(r.area || g.fullScreen || 'Anywhere')}</select><button class="x" title="Remove">✕</button>`
      d.querySelector('.x').onclick = () => d.remove()
      // renaming to a different event starts a new one (keeps the old one's settings out of it)
      d.querySelector('.dn').addEventListener('change', () => { d.dataset.event = '' })
      $('rows').appendChild(d)
      return d
    }
    g.rows.forEach(addRow)
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
      const rows = [...o.querySelectorAll('.pwrs-ev')].map((d) => ({ event: d.dataset.event, displayName: d.querySelector('.dn').value.trim(), words: d.querySelector('.w').value.split(',').map((w) => w.trim()).filter(Boolean), area: d.querySelector('.a').value }))
      $('save').disabled = true; $('st').textContent = 'Saving and reinstalling the game…'
      const r = await post('/__pw/x/edit-save', { id, rows })
      $('save').disabled = false
      $('st').textContent = r.ok ? `Saved: ${r.events} events. New analyses use them straight away.` : 'Could not save: ' + r.error
      if (r.ok) $('reset').disabled = false
    }
  }
  window.powderRsEditGame = openEditor

  window.powderRsAddGame = open

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
      a.onclick = open
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
