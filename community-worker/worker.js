// Rewind community games relay (Cloudflare Worker, free plan).
//
// POST /submit  - Rewind's "Share this game" sends a game here. The worker opens a pull request on
//                 the repo with the files under games/<ID>/. Any reviewer with write access to the
//                 repo can approve and merge it; nothing reaches players before that.
// GET  /index   - the list of approved games, built from the repo (no index.json to maintain).
//
// Secrets / variables (Workers > Settings > Variables):
//   GITHUB_TOKEN  (secret)  fine-grained token: this repo only, Contents + Pull requests = Read and write
//   REPO          (text)    DavidSnipeZ669/powder-rust
//   BRANCH        (text)    main

const MAX_TOTAL = 8 << 20 // 8 MB per game
const ALLOWED = /^(game\.json|events\.json|game_postprocess\.lua|suggestions\.json|[A-Z0-9_]{2,32}\.rwdmdl|assets\/[A-Za-z0-9_.\-\/]{1,100}\.(png|jpg|jpeg|webp|json|svg))$/
const ID = /^[A-Z0-9_]{2,32}$/

export default {
  async fetch(req, env, ctx) {
    const url = new URL(req.url)
    try {
      if (req.method === 'GET' && url.pathname === '/index') return await index(env, ctx)
      if (req.method === 'POST' && url.pathname === '/submit') return await submit(req, env)
      return json({ ok: true, service: 'rewind-community' })
    } catch (e) {
      return json({ ok: false, error: String(e.message || e) }, 500)
    }
  },
}

function json(v, status = 200, extra = {}) {
  return new Response(JSON.stringify(v), { status, headers: { 'content-type': 'application/json', ...extra } })
}

async function gh(env, path, init = {}) {
  const r = await fetch(`https://api.github.com/repos/${env.REPO}${path}`, {
    ...init,
    headers: { authorization: `Bearer ${env.GITHUB_TOKEN}`, accept: 'application/vnd.github+json', 'user-agent': 'rewind-community-worker', 'content-type': 'application/json' },
  })
  if (!r.ok) throw new Error(`GitHub ${path}: ${r.status} ${(await r.text()).slice(0, 200)}`)
  return r.json()
}

async function submit(req, env) {
  // one submission per IP every 5 minutes
  const ip = req.headers.get('cf-connecting-ip') || 'x'
  const cache = caches.default
  const key = new Request(`https://rate.local/${ip}`)
  if (await cache.match(key)) return json({ ok: false, error: 'Please wait a few minutes before sharing another game.' }, 429)

  const body = await req.json()
  const id = String(body.id || '')
  if (!ID.test(id)) return json({ ok: false, error: 'bad game id' }, 400)
  const files = body.files || {}
  let total = 0
  for (const [p, b64] of Object.entries(files)) {
    if (!ALLOWED.test(p) || p.includes('..')) return json({ ok: false, error: `file not allowed: ${p}` }, 400)
    total += (String(b64).length * 3) / 4
  }
  if (!files['events.json'] || !files['game_postprocess.lua']) return json({ ok: false, error: 'events.json and game_postprocess.lua are required' }, 400)
  if (total > MAX_TOTAL) return json({ ok: false, error: 'game is too big (max 8 MB)' }, 400)
  const name = String(body.name || id).slice(0, 80)
  const author = String(body.author || 'anonymous').slice(0, 60)
  const notes = String(body.notes || '').slice(0, 2000)

  const base = await gh(env, `/git/ref/heads/${env.BRANCH}`)
  const baseSha = base.object.sha
  const baseCommit = await gh(env, `/git/commits/${baseSha}`)
  const tree = []
  for (const [p, b64] of Object.entries(files)) {
    const blob = await gh(env, '/git/blobs', { method: 'POST', body: JSON.stringify({ content: b64, encoding: 'base64' }) })
    tree.push({ path: `games/${id}/${p}`, mode: '100644', type: 'blob', sha: blob.sha })
  }
  const t = await gh(env, '/git/trees', { method: 'POST', body: JSON.stringify({ base_tree: baseCommit.tree.sha, tree }) })
  const c = await gh(env, '/git/commits', { method: 'POST', body: JSON.stringify({ message: `Community game: ${name} (${id})`, tree: t.sha, parents: [baseSha] }) })
  const branch = `submit/${id}-${Date.now()}`
  await gh(env, '/git/refs', { method: 'POST', body: JSON.stringify({ ref: `refs/heads/${branch}`, sha: c.sha }) })
  const pr = await gh(env, '/pulls', {
    method: 'POST',
    body: JSON.stringify({
      title: `New game: ${name} (${id})`,
      head: branch,
      base: env.BRANCH,
      body: `Shared from Rewind by **${author}**.\n\nFiles: ${Object.keys(files).map((f) => '`' + f + '`').join(', ')}\n\n${notes}\n\n---\nReviewers: check the events look sensible, then **Approve** and **Merge**. Players get it on their next start.`,
    }),
  })
  await cache.put(key, new Response('1', { headers: { 'cache-control': 'max-age=300' } }))
  return json({ ok: true, url: pr.html_url, number: pr.number })
}

async function index(env, ctx) {
  const cache = caches.default
  const key = new Request('https://index.local/v1')
  const hit = await cache.match(key)
  if (hit) return hit
  const tree = await gh(env, `/git/trees/${env.BRANCH}?recursive=1`)
  const games = {}
  for (const e of tree.tree) {
    const m = /^games\/([A-Z0-9_]{2,32})\/(.+)$/.exec(e.path)
    if (!m || e.type !== 'blob') continue
    const [, id, f] = m
    if (f.split('/').some((s) => s.startsWith('.') || s.startsWith('_'))) continue
    ;(games[id] = games[id] || []).push({ f, sha: e.sha })
  }
  const out = []
  for (const id of Object.keys(games).sort()) {
    const files = games[id].sort((a, b) => (a.f < b.f ? -1 : 1))
    if (!files.some((x) => x.f === 'events.json') || !files.some((x) => x.f === 'game_postprocess.lua')) continue
    const h = await crypto.subtle.digest('SHA-256', new TextEncoder().encode(files.map((x) => x.f + ':' + x.sha).join('\n')))
    const version = [...new Uint8Array(h)].slice(0, 8).map((b) => b.toString(16).padStart(2, '0')).join('')
    let meta = {}
    try {
      meta = await (await fetch(`https://raw.githubusercontent.com/${env.REPO}/${env.BRANCH}/games/${id}/game.json`)).json()
    } catch {}
    out.push({ id, name: meta.name || id, authors: meta.authors || null, version, files: files.map((x) => x.f) })
  }
  const res = json({ format: 1, games: out }, 200, { 'cache-control': 'max-age=600' })
  ctx.waitUntil(cache.put(key, res.clone()))
  return res
}
