// powder-rs: stands in for Electron inside Powder's original interface.
// Powder's own preload.js runs unchanged on top of this; every ipcRenderer call goes to the Rust main process.
(function () {
  const TOKEN = '__PW_TOKEN__';
  const listeners = {};
  const H = { 'content-type': 'application/json', 'x-pw-token': TOKEN };
  const ipcRenderer = {
    invoke: async (channel, ...args) => {
      const r = await fetch('/__pw/invoke', { method: 'POST', headers: H, body: JSON.stringify({ channel, args }) });
      const j = await r.json();
      return j && j.__pwUndefined === 1 ? undefined : j;
    },
    on: (channel, cb) => { (listeners[channel] = listeners[channel] || []).push(cb); return ipcRenderer },
    removeListener: (channel, cb) => { listeners[channel] = (listeners[channel] || []).filter((x) => x !== cb) },
    send: (channel, ...args) => { ipcRenderer.invoke(channel, ...args) },
  };
  const electron = {
    ipcRenderer,
    contextBridge: { exposeInMainWorld: (k, v) => { window[k] = v } },
    webUtils: { getPathForFile: (f) => (f && (f.path || f.__pwPath)) || '' },
  };
  const timers = { setTimeout, clearTimeout, setInterval, clearInterval, setImmediate: (f, ...a) => setTimeout(f, 0, ...a) };
  window.require = (m) => (m === 'electron' ? electron : m === 'timers' ? timers : {});
  window.global = window;
  window.process = window.process || { env: { NODE_ENV: 'production' }, platform: 'win32', versions: { electron: '33.0.0', node: '20.0.0', chrome: '130.0.0' }, argv: [], cwd: () => '/', nextTick: (f, ...a) => setTimeout(() => f(...a), 0), on: () => {} };
  window.module = undefined; window.exports = undefined;

  // events pushed by the main process (variables, streams) via long polling
  let seq = 0;
  async function poll() {
    // start from "now": variables are fetched fresh by preload, old stream events must not replay
    try { seq = (await ipcRenderer.invoke('__pw.seq')) || 0 } catch (e) {}
    for (;;) {
      try {
        const r = await fetch('/__pw/events?since=' + seq, { headers: H });
        const evs = await r.json();
        for (const e of evs) {
          seq = Math.max(seq, e.seq);
          for (const cb of listeners[e.channel] || []) { try { cb({}, e.payload) } catch (err) { console.error(err) } }
        }
      } catch (e) { await new Promise((r) => setTimeout(r, 500)) }
    }
  }
  poll();

  // theme feature (main process in the original): theme__light / theme__dark on <html>
  function applyTheme(t) {
    const resolved = t === 'lighter' ? 'light' : (t || 'dark');
    const cl = document.documentElement.classList;
    for (const c of ['theme__light', 'theme__lighter', 'theme__dark', 'theme__system']) cl.remove(c);
    cl.add('theme__' + resolved);
    window.__pwTheme = t;
  }
  (listeners['__pw.theme'] = listeners['__pw.theme'] || []).push((_e, p) => applyTheme(p && p.value));
  ipcRenderer.invoke('__pw.theme').then(applyTheme);


  // no browser-style reload/back keys (Electron had none either)
  window.addEventListener('keydown', (e) => {
    const k = e.key;
    if (k === 'F5' || ((e.ctrlKey || e.metaKey) && (k === 'r' || k === 'R')) || (e.altKey && (k === 'ArrowLeft' || k === 'ArrowRight')) || k === 'BrowserBack' || k === 'BrowserForward') {
      e.preventDefault();
    }
  }, true);

  // mouse back/forward buttons
  window.addEventListener('mouseup', (e) => { if (e.button === 3 || e.button === 4) e.preventDefault() }, true);
  window.addEventListener('mousedown', (e) => { if (e.button === 3 || e.button === 4) e.preventDefault() }, true);

  // frameless window: Powder's title bar uses -webkit-app-region: drag
  window.addEventListener('mousedown', (e) => {
    if (e.button !== 0) return;
    for (let el = e.target; el && el.nodeType === 1; el = el.parentElement) {
      const r = getComputedStyle(el).getPropertyValue('-webkit-app-region') || getComputedStyle(el).getPropertyValue('app-region');
      if (r === 'no-drag') return;
      if (r === 'drag') { if (window.ipc) window.ipc.postMessage('drag'); else ipcRenderer.invoke('__pw.window.drag'); return }
    }
  }, true);
  window.addEventListener('dblclick', (e) => {
    for (let el = e.target; el && el.nodeType === 1; el = el.parentElement) {
      const r = getComputedStyle(el).getPropertyValue('-webkit-app-region');
      if (r === 'no-drag') return;
      if (r === 'drag') { ipcRenderer.invoke('__pw.window.toggle-maximize'); return }
    }
  }, true);
})();
