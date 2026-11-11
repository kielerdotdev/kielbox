/** Allowed upload types and their fixed object-key extensions (never taken from filenames). */
export const CONTENT_TYPES: Readonly<Record<string, string>> = {
  'image/png': 'png',
  'image/jpeg': 'jpg',
  'image/webp': 'webp',
  'image/gif': 'gif',
  'video/mp4': 'mp4',
  'video/webm': 'webm',
  'video/x-matroska': 'mkv',
  'video/quicktime': 'mov',
}

export function extensionFor(contentType: string): string | null {
  return Object.hasOwn(CONTENT_TYPES, contentType) ? CONTENT_TYPES[contentType]! : null
}

export function isVideo(contentType: string): boolean {
  return contentType.startsWith('video/')
}
