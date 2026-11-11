export interface UploadSigner {
  presignPut(key: string, contentType: string, ttlSeconds: number): Promise<string>
  createMultipart(key: string, contentType: string): Promise<string> // uploadId
  presignPart(key: string, uploadId: string, partNumber: number, ttlSeconds: number): Promise<string>
  completeMultipart(
    key: string,
    uploadId: string,
    parts: { partNumber: number; etag: string }[],
  ): Promise<void>
  abortMultipart(key: string, uploadId: string): Promise<void>
}

/** Thrown by a signer when the storage backend rejects an operation. */
export class StorageError extends Error {
  readonly code: string
  constructor(code: string, message: string) {
    super(message)
    this.name = 'StorageError'
    this.code = code
  }
}
