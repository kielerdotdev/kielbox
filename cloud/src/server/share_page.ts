/** Plain server-rendered share page. No JavaScript, no Tailwind, no Base UI. */
import { isVideo } from './content_types'

export function escapeHtml(text: string): string {
  return text
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
    .replace(/'/g, '&#39;')
}

export const SHARE_PAGE_HEADERS: Readonly<Record<string, string>> = {
  'content-type': 'text/html; charset=utf-8',
  'content-security-policy':
    "default-src 'none'; img-src 'self'; media-src 'self'; style-src 'unsafe-inline'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'",
  'referrer-policy': 'no-referrer',
  'x-robots-tag': 'noindex, nofollow',
  'cache-control': 'private, no-store',
  'x-content-type-options': 'nosniff',
}

// Quiet tokens (crates/desktop/src/rune_theme.rs).
const STYLE = `
:root{color-scheme:dark}
*{box-sizing:border-box}
body{margin:0;min-height:100vh;background:#141414;color:#f2f2f2;font:14px/20px system-ui,-apple-system,"Segoe UI",sans-serif;display:flex;flex-direction:column;align-items:center;justify-content:center;gap:16px;padding:24px}
main{width:100%;max-width:960px;background:#1d1d1d;border-radius:12px;padding:16px;box-shadow:0 0 0 .5px rgba(255,255,255,.12)}
h1{margin:0 0 12px;font-size:20px;line-height:28px;font-weight:600;letter-spacing:-.012em;overflow-wrap:anywhere}
video,img{display:block;width:100%;max-height:75vh;object-fit:contain;background:#000;border-radius:8px}
.meta{margin-top:12px;display:flex;gap:12px;align-items:center;justify-content:space-between;color:#a8a8a8;font-size:12.5px}
a.button{background:#49c970;color:#07210e;text-decoration:none;font-weight:600;padding:8px 14px;border-radius:8px}
a.button:hover{background:#5bd27e}
`

export interface SharePageInput {
  token: string
  origin: string
  title: string
  filename: string
  contentType: string
  allowDownload: boolean
  hasThumb: boolean
}

export function renderSharePage(i: SharePageInput): string {
  const base = `/s/${i.token}`
  const abs = (path: string) => escapeHtml(`${i.origin}${base}${path}`)
  const title = escapeHtml(i.title)
  const video = isVideo(i.contentType)
  const poster = i.hasThumb ? ` poster="${escapeHtml(base)}/thumb"` : ''
  const player = video
    ? `<video controls preload="metadata"${poster} src="${escapeHtml(base)}/media"></video>`
    : `<img src="${escapeHtml(base)}/media" alt="${title}">`
  const og = [
    `<meta property="og:title" content="${title}">`,
    `<meta property="og:type" content="${video ? 'video.other' : 'website'}">`,
    i.hasThumb
      ? `<meta property="og:image" content="${abs('/thumb')}">`
      : video
        ? ''
        : `<meta property="og:image" content="${abs('/media')}">`,
    i.contentType === 'video/mp4'
      ? `<meta property="og:video" content="${abs('/media')}"><meta property="og:video:type" content="video/mp4">`
      : '',
  ]
    .filter(Boolean)
    .join('\n')
  const download = i.allowDownload
    ? `<a class="button" href="${escapeHtml(base)}/media?download=1" download>Download</a>`
    : ''
  return `<!doctype html>
<html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<meta name="robots" content="noindex,nofollow">
<title>${title}</title>
${og}
<style>${STYLE}</style></head>
<body><main>
<h1>${title}</h1>
${player}
<div class="meta"><span>${escapeHtml(i.filename)}</span>${download}</div>
</main></body></html>`
}

export function renderNotFoundPage(): string {
  return `<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<meta name="robots" content="noindex,nofollow"><title>Not found</title><style>${STYLE}</style></head>
<body><main><h1>Link not found</h1><p style="margin:0;color:#a8a8a8">This link does not exist, has expired or was revoked.</p></main></body></html>`
}
