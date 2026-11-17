// A consumer of the package's types: `tsc -p crates/sealwax-wasm/tests/types` compiles it, so
// that a declaration that does not compile fails the package's tests.

import init, {
  Document,
  PdfEncryptedError,
  PdfError,
  SealwaxCrashedError,
  isCrashed,
  mergeBytes,
  reset,
  type Field,
  type RenderedPage,
} from "sealwax";

export async function use(bytes: Uint8Array): Promise<number> {
  await init();
  const doc = Document.fromBytes(bytes);
  const withOptions = Document.fromBytes(bytes, { refuseEncrypted: true, password: "" });
  doc.rotatePages([[0, 1]]);
  const saved: Uint8Array = doc.save();
  const merged: Uint8Array = mergeBytes([saved, withOptions.save()]);
  const fields: Field[] = doc.fields();
  const page: RenderedPage | null = doc.renderRgba(0, 1);
  try {
    doc.flatten();
  } catch (error) {
    if (error instanceof SealwaxCrashedError || isCrashed()) {
      await reset();
    } else if (error instanceof PdfEncryptedError || error instanceof PdfError) {
      return -1;
    }
  }
  return merged.length + fields.length + (page?.width ?? 0);
}
