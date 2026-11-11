/** Item ids, share tokens and hashing. */

const CROCKFORD = '0123456789ABCDEFGHJKMNPQRSTVWXYZ'

/** 26-character ULID: 48-bit millisecond timestamp then 80 random bits. */
export function ulid(now: number = Date.now()): string {
  let time = ''
  let t = now
  for (let i = 0; i < 10; i++) {
    time = CROCKFORD[t % 32] + time
    t = Math.floor(t / 32)
  }
  const rnd = crypto.getRandomValues(new Uint8Array(16))
  let tail = ''
  for (let i = 0; i < 16; i++) tail += CROCKFORD[rnd[i]! & 31]
  return time + tail
}

export const ULID_RE = /^[0-9A-HJKMNP-TV-Z]{26}$/

const B64URL = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_'

export function base64urlEncode(bytes: Uint8Array): string {
  let out = ''
  let i = 0
  for (; i + 2 < bytes.length; i += 3) {
    const n = (bytes[i]! << 16) | (bytes[i + 1]! << 8) | bytes[i + 2]!
    out += B64URL[(n >> 18) & 63]! + B64URL[(n >> 12) & 63]! + B64URL[(n >> 6) & 63]! + B64URL[n & 63]!
  }
  if (i + 1 === bytes.length) {
    const n = bytes[i]! << 16
    out += B64URL[(n >> 18) & 63]! + B64URL[(n >> 12) & 63]!
  } else if (i + 2 === bytes.length) {
    const n = (bytes[i]! << 16) | (bytes[i + 1]! << 8)
    out += B64URL[(n >> 18) & 63]! + B64URL[(n >> 12) & 63]! + B64URL[(n >> 6) & 63]!
  }
  return out
}

/** Strict, canonical unpadded base64url decode; returns null on any irregularity. */
export function base64urlDecode(text: string): Uint8Array | null {
  if (!/^[A-Za-z0-9_-]*$/.test(text) || text.length % 4 === 1) return null
  const out: number[] = []
  let acc = 0
  let bits = 0
  for (const ch of text) {
    acc = (acc << 6) | B64URL.indexOf(ch)
    bits += 6
    if (bits >= 8) {
      bits -= 8
      out.push((acc >> bits) & 0xff)
      acc &= (1 << bits) - 1
    }
  }
  if (acc !== 0) return null // non-canonical trailing bits
  return Uint8Array.from(out)
}

export const TOKEN_BYTES = 32
export const TOKEN_LEN = 43

export function newShareToken(): string {
  return base64urlEncode(crypto.getRandomValues(new Uint8Array(TOKEN_BYTES)))
}

/** Decode a presented token; null unless it is exactly 43 chars of the alphabet encoding 32 bytes. */
export function decodeShareToken(token: string): Uint8Array | null {
  if (token.length !== TOKEN_LEN) return null
  const bytes = base64urlDecode(token)
  return bytes && bytes.length === TOKEN_BYTES ? bytes : null
}

export function toHex(bytes: ArrayBuffer | Uint8Array): string {
  const view = bytes instanceof Uint8Array ? bytes : new Uint8Array(bytes)
  return Array.from(view, (b) => b.toString(16).padStart(2, '0')).join('')
}

export async function sha256Hex(data: Uint8Array | string): Promise<string> {
  const bytes = typeof data === 'string' ? new TextEncoder().encode(data) : data
  return toHex(await crypto.subtle.digest('SHA-256', bytes as BufferSource))
}
