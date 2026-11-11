export const KINDS = ['screenshot', 'recording', 'replay', 'clip', 'other'] as const
export type Kind = (typeof KINDS)[number]

export function formatSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`
  const units = ['KB', 'MB', 'GB', 'TB']
  let v = bytes / 1024
  let i = 0
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024
    i++
  }
  return `${v >= 100 ? v.toFixed(0) : v.toFixed(1)} ${units[i]}`
}

export function formatDuration(ms: number): string {
  const total = Math.round(ms / 1000)
  const h = Math.floor(total / 3600)
  const m = Math.floor((total % 3600) / 60)
  const s = total % 60
  const pad = (n: number) => String(n).padStart(2, '0')
  return h > 0 ? `${h}:${pad(m)}:${pad(s)}` : `${m}:${pad(s)}`
}

const dateFmt = new Intl.DateTimeFormat('en', { year: 'numeric', month: 'short', day: 'numeric', timeZone: 'UTC' })
const monthFmt = new Intl.DateTimeFormat('en', { year: 'numeric', month: 'long', timeZone: 'UTC' })
const stampFmt = new Intl.DateTimeFormat('en', { dateStyle: 'medium', timeStyle: 'short', timeZone: 'UTC' })

export const formatDate = (ms: number) => dateFmt.format(ms)
export const monthLabel = (ms: number) => monthFmt.format(ms)
export const formatStamp = (ms: number) => `${stampFmt.format(ms)} UTC`

export const KIND_LABEL: Record<Kind, string> = {
  screenshot: 'Screenshot',
  recording: 'Recording',
  replay: 'Replay',
  clip: 'Clip',
  other: 'Other',
}

const shortDateFmt = new Intl.DateTimeFormat('en-GB', { month: 'short', day: 'numeric', timeZone: 'UTC' })
export const formatShortDate = (ms: number) => shortDateFmt.format(ms)

const linkStampFmt = new Intl.DateTimeFormat('en-GB', { day: 'numeric', month: 'short', hour: '2-digit', minute: '2-digit', timeZone: 'UTC' })
export const formatLinkStamp = (ms: number) => linkStampFmt.format(ms)
