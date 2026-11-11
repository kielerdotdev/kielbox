/** Plain server-rendered share page. No JavaScript, no Tailwind, no Base UI. */
import { formatDuration, formatSize } from '../lib/format'
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
    "default-src 'none'; img-src 'self'; media-src 'self'; style-src 'unsafe-inline'; font-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'",
  'referrer-policy': 'no-referrer',
  'x-robots-tag': 'noindex, nofollow',
  'cache-control': 'private, no-store',
  'x-content-type-options': 'nosniff',
}

const BRAND = `<span class="brand"><svg width="16" height="16" viewBox="0 0 24 24" aria-hidden="true"><rect x="2" y="2" width="20" height="10" rx="3.2" fill="#49c970"/><path d="M10.4 4.6v4.8l4-2.4z" fill="#141414"/><rect x="2" y="13.8" width="9.1" height="8.2" rx="3" fill="#f2f2f2"/><rect x="12.9" y="13.8" width="9.1" height="8.2" rx="3" fill="#f2f2f2"/></svg>Shared with Kielbox</span>`
const STYLE = `
@font-face{font-family:Geist;src:url('/fonts/Geist.ttf');font-weight:100 900;font-display:swap}
:root{color-scheme:dark;scrollbar-color:#444 #141414}
*{box-sizing:border-box}
body{margin:0;min-height:100vh;background:#141414;color:#f2f2f2;font:440 14px/20px Geist,system-ui,-apple-system,"Segoe UI",sans-serif;display:flex;flex-direction:column;padding:clamp(16px,3vw,40px) clamp(16px,3vw,40px) 24px}
main{width:100%;max-width:960px;margin:0 auto;display:flex;flex-direction:column;gap:18px}
h1{margin:0;font-size:20px;line-height:28px;font-weight:600;letter-spacing:-.012em;overflow-wrap:anywhere;text-wrap:pretty}
video,img{display:block;width:100%;max-height:75vh;object-fit:contain;background:#0e0e0e;border-radius:8px;box-shadow:0 0 0 .5px rgba(255,255,255,.1)}
video{aspect-ratio:16/9}
.meta{display:flex;flex-wrap:wrap;gap:16px 24px;align-items:center}
.caption{flex:1 1 260px;min-width:0}
.caption p{margin:2px 0 0;color:#a8a8a8;font-size:12.5px;line-height:18px;font-variant-numeric:tabular-nums}
a.button{display:inline-flex;align-items:center;gap:7px;height:28px;padding:0 10px;background:rgba(255,255,255,.06);color:#f2f2f2;text-decoration:none;font-size:13px;font-weight:530;border-radius:6px;box-shadow:0 1px 2px rgba(0,0,0,.4),0 0 0 .5px rgba(255,255,255,.18)}
a.button:hover{background:rgba(255,255,255,.1)}
footer{margin-top:auto;padding-top:48px;display:flex;justify-content:center}
.brand{display:inline-flex;align-items:center;gap:8px;color:#8f8f8f;font-size:12.5px;line-height:18px}
.unavailable{flex:1;align-items:center;justify-content:center;text-align:center;gap:8px}
.unavailable p{max-width:290px;margin:0;color:#a8a8a8;text-wrap:pretty}
.unavailable>svg{color:#8f8f8f;margin-bottom:4px}
*:focus-visible{outline:2px solid #49c970;outline-offset:4px}
::selection{background:rgba(73,201,112,.28)}
`

export interface SharePageInput {
  token: string
  origin: string
  title: string
  filename: string
  contentType: string
  allowDownload: boolean
  size?: number
  width?: number | null
  height?: number | null
  durationMs?: number | null
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
    ? `<a class="button" href="${escapeHtml(base)}/media?download=1" download><svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M12 4v11m-4.5-4.5L12 15l4.5-4.5M5 20h14"/></svg>Download</a>`
    : ''
  const details = [video ? 'Video' : 'Image', video && i.durationMs != null ? formatDuration(i.durationMs) : !video && i.width && i.height ? `${i.width} × ${i.height}` : null, i.size != null ? formatSize(i.size) : null].filter(Boolean).join(' · ')
  return `<!doctype html>
<html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<meta name="robots" content="noindex,nofollow">
<title>${title}</title>
${og}
<style>${STYLE}</style></head>
<body><main>
${player}
<div class="meta"><div class="caption"><h1>${title}</h1><p title="${escapeHtml(i.filename)}">${escapeHtml(details)}</p></div>${download}</div>
</main><footer>${BRAND}</footer></body></html>`
}

export function renderNotFoundPage(): string {
  return `<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<meta name="robots" content="noindex,nofollow"><title>This link isn't available</title><style>${STYLE}</style></head>
<body><main class="unavailable"><svg width="28" height="28" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="m18.84 12.25 1.72-1.71a5 5 0 0 0-7.07-7.07l-1.72 1.71M5.17 11.75l-1.71 1.71a5 5 0 0 0 7.07 7.07l1.71-1.71M8 2v3M2 8h3M16 19v3M19 16h3"/></svg><h1>This link isn't available</h1><p>It may have expired or been turned off. Ask the person who sent it for a new link.</p></main><footer>${BRAND}</footer></body></html>`
}
