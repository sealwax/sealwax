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
  xfdfFor,
  type Applied,
  type Drawings,
  type Glyph,
  type Link,
  type Attachment,
  type TextWord,
  type PageImage,
  type ExtractedImage,
  type Field,
  type SignedMark,
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
  const drawn: Drawings = doc.drawings(0, true);
  const links: Link[] = doc.links(0);
  const firstPage: number | null = links[0]?.kind === "page" ? links[0].pageNumber : null;
  const labels: string[] | null = doc.pageLabels();
  doc.addAttachment("a.txt", new Uint8Array([97]), { mimeType: "text/plain" });
  const pageWords: TextWord[] = doc.words(0);
  const files: Attachment[] = doc.attachments();
  const first: Uint8Array | null = files.length > 0 ? doc.attachmentData(0) : null;
  const pictures: PageImage[] = doc.images(0);
  const picture: ExtractedImage | null = pictures.length > 0 ? doc.extractImage(0, 0) : null;
  const boxes = drawn.rectangles.filter((r) => r.stroked && !r.filled).map((r) => r.box);
  const glyphs: Glyph[] = doc.glyphs(0);
  const text = glyphs.map((g) => g.text ?? "").join("") + (glyphs[0]?.ink?.[0] ?? 0);
  const applied: Applied = doc.applyXfdf("<xfdf/>", { flatten: false, removeAnnotations: true });
  const changed: number[] = applied.pages;
  const marks: SignedMark[] = [
    { kind: "freetext", pageIndex: 0, rect: [0, 0, 10, 10], name: "t", text: "x", rotation: 90 },
    { kind: "ink", pageIndex: 0, name: "i", strokes: [[[1, 2], [3, 4]]], width: 2 },
  ];
  const xfdf: string = xfdfFor(marks, { deleted: [[0, "p"]] });
  doc.applyXfdf(xfdf, { fonts: { Caveat: bytes }, defaultFont: bytes });
  try {
    doc.flatten();
  } catch (error) {
    if (error instanceof SealwaxCrashedError || isCrashed()) {
      await reset();
    } else if (error instanceof PdfEncryptedError || error instanceof PdfError) {
      return -1;
    }
  }
  return (
    merged.length + fields.length + (page?.width ?? 0) + changed.length + xfdf.length + boxes.length + text.length +
    (firstPage ?? 0) + (labels?.length ?? 0) + (picture?.image.length ?? 0) + (first?.length ?? 0) + pageWords.length
  );
}
