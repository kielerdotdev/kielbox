import type { ItemJson } from '../functions/types'

/** Points straight at the same-origin media route, so the Access cookie and Range seeking just work. */
export function Player({ item }: { item: ItemJson }) {
  if (item.content_type.startsWith('video/')) {
    return (
      <video
        controls
        preload="metadata"
        poster={item.thumb_url ?? undefined}
        src={item.media_url}
        className="capture-player"
      />
    )
  }
  return <img src={item.media_url} alt={item.title} className="capture-player object-contain" />
}
