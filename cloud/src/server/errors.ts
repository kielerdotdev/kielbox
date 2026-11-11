export type ErrorCode =
  | 'unauthorized'
  | 'forbidden'
  | 'not_found'
  | 'invalid_request'
  | 'too_large'
  | 'unsupported_type'
  | 'conflict'
  | 'upload_incomplete'
  | 'internal'

const STATUS: Record<ErrorCode, number> = {
  unauthorized: 401,
  forbidden: 403,
  not_found: 404,
  invalid_request: 400,
  too_large: 413,
  unsupported_type: 415,
  conflict: 409,
  upload_incomplete: 409,
  internal: 500,
}

/** A failure with a stable code and a message that is safe to show to callers. */
export class HttpError extends Error {
  readonly code: ErrorCode
  readonly status: number
  constructor(code: ErrorCode, message: string, status?: number) {
    super(message)
    this.name = 'HttpError'
    this.code = code
    this.status = status ?? STATUS[code]
  }
}

export function errorResponse(code: ErrorCode, message: string, status?: number): Response {
  return new Response(JSON.stringify({ error: { code, message } }), {
    status: status ?? STATUS[code],
    headers: {
      'content-type': 'application/json; charset=utf-8',
      'cache-control': 'no-store',
      'x-content-type-options': 'nosniff',
    },
  })
}
