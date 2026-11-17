// The errors sealwax throws. The WebAssembly module builds them from here, so that `instanceof`
// works on what a caller catches; the package re-exports this module.

/** Anything sealwax refuses or cannot do with a document. */
export class PdfError extends Error {
  constructor(message) {
    super(message);
    this.name = new.target.name;
  }
}

/** The bytes are not a PDF sealwax can read. */
export class PdfReadError extends PdfError {}

/** The document is encrypted and its password was not given, or is wrong. */
export class PdfPasswordError extends PdfError {}

/** The document is encrypted, and encrypted documents were refused (`refuseEncrypted`). */
export class PdfEncryptedError extends PdfReadError {}

/** A bound of the limits was passed; `limit` names it ("max_operators", ...). */
export class PdfLimitError extends PdfError {
  constructor(message, limit) {
    super(message);
    this.limit = limit;
  }
}

/** The deadline passed; `limit` is "deadline". */
export class PdfTimeoutError extends PdfLimitError {}

/** The XFDF cannot be applied. */
export class XfdfError extends PdfError {}

/** A field to create is named as one the form already has. */
export class FieldNameTakenError extends PdfError {}

/**
 * The WebAssembly module stopped, a panic aborting it: every call throws this until
 * \`reset()\`, and the documents opened before are lost. Not a \`PdfError\`: the document is
 * not the cause it names.
 */
export class SealwaxCrashedError extends Error {
  constructor(cause, message = "sealwax stopped: call reset() and open the documents again") {
    super(message, cause === undefined ? undefined : { cause });
    this.name = "SealwaxCrashedError";
  }
}

const KINDS = {
  read: PdfReadError,
  password: PdfPasswordError,
  encrypted: PdfEncryptedError,
  limit: PdfLimitError,
  timeout: PdfTimeoutError,
  xfdf: XfdfError,
  name: FieldNameTakenError,
};

/** Called by the WebAssembly module: the error of `kind`, never quoting a document's content. */
export function makeError(kind, message, limit) {
  if (kind === "argument") {
    return new RangeError(message);
  }
  const Kind = KINDS[kind] ?? PdfError;
  return new Kind(message, limit);
}
