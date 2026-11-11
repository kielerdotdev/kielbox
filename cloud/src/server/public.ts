/** Share-host handler: serves only /s/* (plans/cloud.md section 5.2). */
import type { Services } from './context'
import { streamObject } from './media'
import { recordView } from './db/shares'
import { renderNotFoundPage, renderSharePage, SHARE_PAGE_HEADERS } from './share_page'
import { resolveShare } from './services/shares'

export function notFoundPage(): Response {
  return new Response(renderNotFoundPage(), { status: 404, headers: SHARE_PAGE_HEADERS })
}

const ROUTE = /^\/s\/([^/]+)(?:\/(media|thumb))?$/

export async function handlePublic(
  request: Request,
  svc: Services,
  exec: Pick<ExecutionContext, 'waitUntil'>,
): Promise<Response> {
  if (request.method !== 'GET' && request.method !== 'HEAD') return notFoundPage()
  const url = new URL(request.url)
  const m = ROUTE.exec(url.pathname)
  if (!m) return notFoundPage()
  const token = m[1]!
  const part = m[2]

  const share = await resolveShare(svc, token)
  if (!share) return notFoundPage()
  const item = share.item

  if (part === 'media') {
    const download = url.searchParams.get('download') === '1'
    if (download && share.allow_download !== 1) {
      return new Response('downloads are disabled for this link', {
        status: 403,
        headers: { 'content-type': 'text/plain; charset=utf-8', 'cache-control': 'no-store' },
      })
    }
    return streamObject(svc.bucket, request, {
      key: item.object_key,
      contentType: item.content_type,
      filename: item.filename,
      download,
      cacheControl: 'private, no-store',
    })
  }
  if (part === 'thumb') {
    if (!item.thumb_key) return notFoundPage()
    return streamObject(svc.bucket, request, {
      key: item.thumb_key,
      contentType: 'image/jpeg',
      filename: 'thumbnail.jpg',
      download: false,
      cacheControl: 'private, no-store',
    })
  }

  // The page itself. Count the view out of band so a failed update never fails the view.
  exec.waitUntil(recordView(svc.db, share.id, svc.now()).catch(() => {}))
  const html = renderSharePage({
    token,
    origin: svc.shareOrigin,
    title: item.title,
    filename: item.filename,
    contentType: item.content_type,
    allowDownload: share.allow_download === 1,
    hasThumb: item.thumb_key !== null,
  })
  return new Response(request.method === 'HEAD' ? null : html, { status: 200, headers: SHARE_PAGE_HEADERS })
}
