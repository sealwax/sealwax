/** Anything sealwax refuses or cannot do with a document. */
export class PdfError extends Error {}
/** The bytes are not a PDF sealwax can read. */
export class PdfReadError extends PdfError {}
/** The document is encrypted and its password was not given, or is wrong. */
export class PdfPasswordError extends PdfError {}
/** A bound of the limits was passed; `limit` names it ("max_operators", ...). */
export class PdfLimitError extends PdfError {
  readonly limit: string;
}
/** The deadline passed; `limit` is "deadline". */
export class PdfTimeoutError extends PdfLimitError {}
/** The XFDF cannot be applied. */
export class XfdfError extends PdfError {}
/** A field to create is named as one the form already has. */
export class FieldNameTakenError extends PdfError {}
/** The document is encrypted, and encrypted documents were refused (`refuseEncrypted`). */
export class PdfEncryptedError extends PdfReadError {}
/**
 * The WebAssembly module stopped, a panic aborting it: every call throws this until `reset()`,
 * and the documents opened before are lost.
 */
export class SealwaxCrashedError extends Error {
  constructor(cause?: unknown, message?: string);
}
