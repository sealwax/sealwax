// The npm package as a caller uses it, run by Node's test runner on what build.sh wrote:
//
//   crates/sealwax-wasm/build.sh && node --test crates/sealwax-wasm/tests/package.test.mjs
//
// Documents are built here, never read from a corpus.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";

const pkg = new URL("../pkg/", import.meta.url);
const sealwax = await import(new URL("index.js", pkg));
const { Document, PdfError, PdfLimitError, PdfReadError, PdfTimeoutError, mergeBytes } = sealwax;
await sealwax.default({ module_or_path: readFileSync(new URL("sealwax_bg.wasm", pkg)) });

/** A PDF of `objects` (numbered from 1, the first the catalog) with an exact xref table. */
function build(objects, trailer = "") {
  const encoder = new TextEncoder();
  const parts = [encoder.encode("%PDF-1.7\n")];
  let length = parts[0].length;
  const offsets = [];
  objects.forEach((body, index) => {
    offsets.push(length);
    const part = encoder.encode(`${index + 1} 0 obj\n${body}\nendobj\n`);
    parts.push(part);
    length += part.length;
  });
  const xref = length;
  let tail = `xref\n0 ${objects.length + 1}\n0000000000 65535 f \n`;
  for (const offset of offsets) {
    tail += `${String(offset).padStart(10, "0")} 00000 n \n`;
  }
  tail += `trailer\n<< /Size ${objects.length + 1} /Root 1 0 R ${trailer} >>\nstartxref\n${xref}\n%%EOF\n`;
  parts.push(encoder.encode(tail));
  const out = new Uint8Array(parts.reduce((n, p) => n + p.length, 0));
  let at = 0;
  for (const part of parts) {
    out.set(part, at);
    at += part.length;
  }
  return out;
}

function stream(content) {
  return `<< /Length ${content.length} >>\nstream\n${content}\nendstream`;
}

/** One page with a text field `name` and a check box `agree`. */
function form() {
  return build([
    "<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [4 0 R 5 0 R] /DA (/Helv 0 Tf 0 g) >> >>",
    "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
    "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [4 0 R 5 0 R] /Contents 6 0 R >>",
    "<< /Type /Annot /Subtype /Widget /FT /Tx /T (name) /Rect [100 700 300 720] /P 3 0 R /F 4 >>",
    "<< /Type /Annot /Subtype /Widget /FT /Btn /T (agree) /Rect [100 650 112 662] /P 3 0 R /F 4 " +
      "/AP << /N << /Yes 7 0 R /Off 7 0 R >> >> /AS /Off >>",
    stream("0 0 m 100 100 l S"),
    "<< /Type /XObject /Subtype /Form /BBox [0 0 12 12] /Length 0 >>\nstream\n\nendstream",
  ]);
}

test("a document opens, says what it is, and saves", () => {
  const doc = Document.fromBytes(form());
  assert.equal(doc.pageCount(), 1);
  const [page] = doc.geometry();
  assert.deepEqual(page, {
    pageNumber: 1,
    cropBox: [0, 0, 612, 792],
    mediaBox: [0, 0, 612, 792],
    rotation: 0,
    userUnit: 1,
  });
  assert.equal(doc.metadata().hasForms, true);
  const saved = doc.save();
  assert.ok(saved instanceof Uint8Array);
  assert.equal(new TextDecoder().decode(saved.slice(0, 5)), "%PDF-");
  assert.equal(Document.fromBytes(saved, { pageCount: [1, 1] }).pageCount(), 1);
  assert.ok(doc.save({ xref: "table" }).length > 0);
});

test("fields are read, filled and read back", () => {
  const doc = Document.fromBytes(form());
  const fields = doc.fields();
  assert.deepEqual(
    fields.map((f) => [f.name, f.kind, f.pageNumber]),
    [
      ["name", "text", 1],
      ["agree", "checkbox", 1],
    ],
  );
  assert.deepEqual(fields[1].buttonStates, [new TextEncoder().encode("Off"), new TextEncoder().encode("Yes")]);
  const filled = doc.fillFields({ name: "Élodie", agree: new TextEncoder().encode("Yes"), nobody: "x" });
  assert.deepEqual(filled, {
    occurrences: 2,
    charactersReplaced: 0,
    refused: ["nobody"],
    kept: [],
    written: { agree: { value: "Yes", previous: "Off" }, name: { value: "Élodie", previous: "" } },
  });
  assert.ok(doc.isModified());
  const reopened = Document.fromBytes(doc.save());
  assert.deepEqual(reopened.fieldValues(), { name: ["Élodie"], agree: ["Yes"] });
  assert.deepEqual(reopened.fillFields({ agree: "Maybe" }).refused, ["agree"]);
  assert.equal(reopened.fillFields({ agree: false }).written.agree.value, "Off");
  assert.equal(reopened.fillFields({ agree: true }).written.agree.value, "Yes");
  // Kept: a field holding another value; written: one given the value it holds.
  const kept = reopened.fillFields({ name: "Lise", agree: "Yes" }, { keepFilled: true });
  assert.deepEqual([kept.kept, Object.keys(kept.written)], [["name"], ["agree"]]);
  const again = reopened.fillFields({ name: "Lise" });
  assert.deepEqual(reopened.unfillFields(again.written), []);
  assert.deepEqual(reopened.fieldValues().name, ["Élodie"]);
  assert.throws(() => reopened.unfillFields({ name: { value: "x" } }), RangeError);
  assert.equal(reopened.templateContent().fields, 2);
  assert.deepEqual(reopened.clearTemplate({ title: "Modèle" }), { fields: 2, annotationsRemoved: 0 });
  assert.deepEqual(Document.fromBytes(reopened.save()).fieldValues(), { name: [""], agree: ["Off"] });
});

test("a page renders to PNG, pages merge and are taken out", () => {
  const doc = Document.fromBytes(form());
  const png = doc.renderPng(0, 0.25);
  assert.deepEqual([...png.slice(1, 4)], [0x50, 0x4e, 0x47]);
  const merged = Document.fromBytes(mergeBytes([form(), form()]));
  assert.equal(merged.pageCount(), 2);
  assert.equal(Document.fromBytes(merged.extractPages(1, 2)).pageCount(), 1);
  assert.equal(Document.fromBytes(sealwax.flattenBytes(form())).fields().length, 0);
});

test("failures are the package's own errors, without a document's content", () => {
  assert.throws(() => Document.fromBytes(new TextEncoder().encode("not a pdf")), (error) => {
    assert.ok(error instanceof PdfReadError && error instanceof PdfError && error instanceof Error);
    assert.equal(error.name, "PdfReadError");
    return true;
  });
  assert.throws(() => Document.fromBytes(form()).renderPng(3, 1), RangeError);
  const busy = build([
    "<< /Type /Catalog /Pages 2 0 R >>",
    "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
    "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Contents 4 0 R >>",
    stream("q q q Q Q Q"),
  ]);
  const limited = Document.fromBytes(busy, { limits: { maxOperators: 3 } });
  assert.throws(() => limited.renderPng(0, 1), (error) => {
    assert.ok(error instanceof PdfLimitError);
    assert.equal(error.limit, "max_operators");
    return true;
  });
  assert.throws(() => Document.fromBytes(busy, { limits: { maxOperators: -1 } }), RangeError);
});

test("the deadline holds without the clock panicking", async () => {
  const doc = Document.fromBytes(form(), { limits: { timeoutMs: 1 } });
  await new Promise((resolve) => setTimeout(resolve, 5));
  assert.throws(() => doc.renderPng(0, 1), (error) => {
    assert.ok(error instanceof PdfTimeoutError && error instanceof PdfLimitError);
    assert.equal(error.limit, "deadline");
    return true;
  });
  // A generous one lets the work through.
  assert.ok(Document.fromBytes(form(), { limits: { timeoutMs: 60_000 } }).renderPng(0, 0.1).length > 0);
});

test("the package names its version", () => {
  assert.match(sealwax.version(), /^\d+\.\d+\.\d+/);
});

/** Three pages; field `name` has a widget on pages 1 and 2, field `city` one on page 3. */
function threePages() {
  const widget = (parent, page) =>
    `<< /Type /Annot /Subtype /Widget /Parent ${parent} 0 R /P ${page} 0 R /Rect [10 10 110 30] /F 4 >>`;
  const page = (contents, annot) =>
    `<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 400] /Contents ${contents} 0 R /Annots [${annot} 0 R] >>`;
  return build([
    "<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [9 0 R 12 0 R] /DA (/Helv 0 Tf 0 g) >> >>",
    "<< /Type /Pages /Kids [3 0 R 4 0 R 5 0 R] /Count 3 >>",
    page(6, 10),
    page(7, 11),
    page(8, 13),
    stream("BT (p0) Tj ET"),
    stream("BT (p1) Tj ET"),
    stream("BT (p2) Tj ET"),
    "<< /FT /Tx /T (name) /Kids [10 0 R 11 0 R] >>",
    widget(9, 3),
    widget(9, 4),
    "<< /FT /Tx /T (city) /Kids [13 0 R] >>",
    widget(12, 5),
  ]);
}

test("pages are arranged, inserted and merged with their forms", () => {
  const doc = Document.fromBytes(threePages());
  const arranged = Document.fromBytes(doc.arrange([[2, 1], 0]));
  assert.deepEqual(arranged.geometry().map((g) => g.rotation), [90, 0]);
  assert.deepEqual(arranged.fields().map((f) => [f.name, f.pageNumber]), [
    ["city", 1],
    ["name", 2],
  ]);
  assert.throws(() => doc.arrange([]), RangeError);
  assert.throws(() => doc.arrange([0, 0]), RangeError);
  const inserted = Document.fromBytes(doc.insertPages(3, [threePages()]));
  assert.equal(inserted.pageCount(), 6);
  assert.deepEqual(
    inserted.fields().map((f) => f.name),
    ["name", "name", "city", "name (2)", "name (2)", "city (2)"],
  );
  const merged = Document.fromBytes(mergeBytes([threePages(), threePages()], { flatten: false }));
  assert.equal(merged.fields().length, 6);
  assert.equal(Document.fromBytes(mergeBytes([threePages(), threePages()])).fields().length, 0);
});

test("what goes with a flattening another engine makes", () => {
  const legal = build([
    "<< /Type /Catalog /Pages 2 0 R >>",
    "<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 612 1008] >>",
    "<< /Type /Page /Parent 2 0 R >>",
  ]);
  const doc = Document.fromBytes(legal);
  assert.equal(doc.ownPageBoxes(), 1);
  const marked = build([
    "<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [4 0 R] >> >>",
    "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
    "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Annots [4 0 R 5 0 R] >>",
    "<< /Type /Annot /Subtype /Widget /FT /Tx /T (name) /V (FIELDVALUE) /Rect [10 10 100 30] /F 2 >>",
    "<< /Type /Annot /Subtype /Redact /Rect [0 0 10 10] >>",
  ]);
  assert.equal(sealwax.hasRedactionMarks(marked), true);
  assert.equal(sealwax.hasRedactionMarks(new Uint8Array([1, 2])), true);
  const rebuilt = Document.fromBytes(Document.fromBytes(marked).rebuiltForRedaction());
  assert.equal(new TextDecoder().decode(rebuilt.save({ xref: "table" })).includes("FIELDVALUE"), false);
  const flattened = Document.fromBytes(marked);
  assert.equal(flattened.clearFlattenedForm(), 1);
});

test("pages render to canvas pixels, turned, and a cancelled rendering draws nothing", () => {
  const page = build([
    "<< /Type /Catalog /Pages 2 0 R >>",
    "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
    "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Contents 4 0 R >>",
    stream("1 0 0 rg 0 0 100 100 re f"),
  ]);
  const doc = Document.fromBytes(page);
  const image = doc.renderRgba(0, 1);
  assert.equal(image.width, 200);
  assert.equal(image.height, 100);
  assert.ok(image.pixels instanceof Uint8ClampedArray);
  assert.equal(image.pixels.length, 200 * 100 * 4);
  // Red on the left half, white on the right, opaque.
  assert.deepEqual([...image.pixels.slice(0, 4)], [255, 0, 0, 255]);
  assert.deepEqual([...image.pixels.slice(199 * 4, 200 * 4)], [255, 255, 255, 255]);
  const turned = doc.renderRgba(0, 1, { quarterTurns: 1 });
  assert.equal(turned.width, 100);
  assert.equal(turned.height, 200);
  const token = new Int32Array(new SharedArrayBuffer(4));
  Atomics.store(token, 0, 1);
  assert.equal(doc.renderRgba(0, 1, { cancel: token }), null);
  Atomics.store(token, 0, 0);
  assert.notEqual(doc.renderRgba(0, 1, { cancel: token }), null);
  assert.throws(() => doc.renderRgba(0, 1, { quarterTurns: 0.5 }), RangeError);
});

test("fields are created and a name taken is refused with its own error", () => {
  const flat = build([
    "<< /Type /Catalog /Pages 2 0 R >>",
    "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
    "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] >>",
  ]);
  const doc = Document.fromBytes(flat);
  doc.addField({ name: "name", kind: "text", pageIndex: 0, rect: [100, 100, 280, 120], required: true });
  doc.addField({ name: "agree", kind: "checkbox", pageIndex: 0, rect: [100, 150, 114, 164] });
  assert.throws(
    () => doc.addField({ name: "name", kind: "date", pageIndex: 0, rect: [10, 10, 100, 30] }),
    (error) => error instanceof sealwax.FieldNameTakenError && error instanceof PdfError,
  );
  assert.throws(() => doc.addField({ name: "x", kind: "radio", pageIndex: 0, rect: [0, 0, 1, 1] }), RangeError);
  doc.addField({ name: "sized", kind: "text", pageIndex: 0, rect: [100, 200, 280, 220], fontSize: 12 });
  assert.ok(new TextDecoder().decode(doc.save({ xref: "table" })).includes("/DA (/Helv 12 Tf 0 g)"));
  for (const fontSize of [0, 1001, Number.NaN, "12"]) {
    assert.throws(
      () => doc.addField({ name: "bad", kind: "text", pageIndex: 0, rect: [10, 10, 100, 30], fontSize }),
      RangeError,
    );
  }
  const reopened = Document.fromBytes(doc.save());
  assert.deepEqual(reopened.fields().map((f) => [f.name, f.kind, f.rect]), [
    ["name", "text", [100, 680, 280, 700]],
    ["agree", "checkbox", [100, 636, 114, 650]],
    ["sized", "text", [100, 580, 280, 600]],
  ]);
});

test("a field is given another widget, on another page, one value for both", () => {
  const twoPages = build([
    "<< /Type /Catalog /Pages 2 0 R >>",
    "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>",
    "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] >>",
    "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] >>",
  ]);
  const doc = Document.fromBytes(twoPages);
  doc.addField({ name: "name", kind: "text", pageIndex: 0, rect: [20, 20, 180, 50], required: true });
  doc.addWidget("name", 1, [20, 20, 180, 50]);
  const built = Document.fromBytes(doc.save());
  assert.deepEqual(built.fields().map((f) => [f.name, f.pageNumber, f.required]), [["name", 1, true], ["name", 2, true]]);
  assert.equal(built.fillFields({ name: "Élodie" }).occurrences, 2);
  assert.throws(() => doc.addWidget("nobody", 1, [20, 20, 180, 50]), RangeError);
  assert.throws(() => doc.addWidget("name", 1, [20, 20]), RangeError);
});

test("photos become pages, those that cannot be read are named", () => {
  const data = new URL("../../sealwax-annots/tests/data/", import.meta.url);
  const jpeg = new Uint8Array(readFileSync(new URL("photo-40x20-exif6.jpg", data)));
  const png = new Uint8Array(readFileSync(new URL("photo-12x10-alpha.png", data)));
  const { pdf, unreadable, added } = sealwax.photosToPdf([jpeg, new Uint8Array([1, 2]), png]);
  assert.deepEqual([added, unreadable], [2, [1]]);
  const doc = Document.fromBytes(pdf);
  assert.deepEqual(doc.geometry().map((page) => page.rotation), [90, 0]);
  assert.deepEqual(doc.addPhotoPages([png]), { added: 1, unreadable: [] });
  assert.throws(() => sealwax.photosToPdf([new Uint8Array([1])]), RangeError);
  assert.throws(() => sealwax.photosToPdf(["text"]), RangeError);
});

test("annotations are made with their appearance, and bad ones refused", () => {
  const blank = build([
    "<< /Type /Catalog /Pages 2 0 R >>",
    "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
    "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] >>",
  ]);
  const doc = Document.fromBytes(blank);
  doc.addAnnotation({ pageIndex: 0, kind: "circle", rect: [10, 10, 90, 90], fill: [0, 0, 1], width: 0 });
  doc.addAnnotation({ pageIndex: 0, kind: "ink", strokes: [[[110, 50], [190, 50]]], color: [0, 1, 0], width: 6 });
  doc.addAnnotation({ pageIndex: 0, kind: "freetext", rect: [110, 70, 190, 95], text: "Note" });
  assert.throws(() => doc.addAnnotation({ pageIndex: 0, kind: "highlight", quads: [[1, 2, 3]] }), RangeError);
  assert.throws(() => doc.addAnnotation({ pageIndex: 3, kind: "square", rect: [0, 0, 1, 1] }), RangeError);
  const image = Document.fromBytes(doc.save()).renderRgba(0, 1);
  const at = (x, y) => [...image.pixels.slice(4 * (y * image.width + x), 4 * (y * image.width + x) + 3)];
  assert.deepEqual(at(50, 50), [0, 0, 255]);
  assert.deepEqual(at(150, 50), [0, 255, 0]);
});

test("redaction marks are applied and areas redacted", () => {
  const marked = build([
    "<< /Type /Catalog /Pages 2 0 R >>",
    "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
    "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> /Annots [6 0 R] >>",
    stream("BT /F1 12 Tf 72 700 Td (Hello secret world) Tj ET"),
    "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>",
    "<< /Type /Annot /Subtype /Redact /Rect [104 690 134 715] /IC [0 0 0] >>",
  ]);
  const doc = Document.fromBytes(marked);
  const applied = doc.applyRedactions();
  assert.equal(applied.marks, 1);
  assert.equal(applied.redacted.glyphs, 6);
  // An incremental save would keep the bytes it was opened from, and what was taken out.
  assert.throws(() => doc.saveIncremental());
  const rebuilt = Document.fromBytes(doc.rebuiltForRedaction());
  assert.equal(new TextDecoder().decode(rebuilt.save({ xref: "table" })).includes("secret"), false);
  assert.deepEqual(rebuilt.redact(0, [[60, 690, 80, 715]], { images: "keep" }).glyphs, 1);
  assert.throws(() => rebuilt.redact(0, [[1, 2]]), RangeError);
});

test("a page's lines, boxes and glyphs are read, each shape with how it is painted", () => {
  const page = build([
    "<< /Type /Catalog /Pages 2 0 R >>",
    "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
    "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> " +
      "/Annots [6 0 R] >>",
    stream(
      "100 600 200 20 re S 100 560 200 1.5 re f 100 500 20 20 re B 100 400 m 300 400 l S " +
        "q 0.866025 0.5 -0.5 0.866025 400 100 cm 0 0 10 20 re S Q " +
        "BT /F1 12 Tf 100 700 Td (Name:) Tj ET",
    ),
    "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>",
    "<< /Type /Annot /Subtype /Square /Rect [10 10 50 50] /AP << /N 7 0 R >> >>",
    "<< /Type /XObject /Subtype /Form /BBox [10 10 50 50] /Length 18 >>\nstream\n10 10 40 40 re S\nendstream",
  ]);
  const doc = Document.fromBytes(page);
  const found = doc.drawings(0);
  assert.deepEqual(found.rectangles, [
    { filled: false, stroked: true, box: [100, 600, 300, 620] },
    { filled: true, stroked: false, box: [100, 560, 300, 561.5] },
    { filled: true, stroked: true, box: [100, 500, 120, 520] },
  ]);
  assert.equal(found.lines.length, 1);
  const [line] = found.lines;
  assert.deepEqual([line.from, line.to, line.stroked, line.filled], [[100, 400], [300, 400], true, false]);
  assert.equal(typeof line.subpath, "number");
  assert.equal(found.quads.length, 1);
  assert.equal(found.quads[0].corners.length, 4);
  assert.equal(found.quads[0].stroked, true);
  // The annotation's appearance, when asked for.
  assert.equal(doc.drawings(0, true).rectangles.length, 4);

  const glyphs = doc.glyphs(0);
  assert.equal(glyphs.map((g) => g.text).join(""), "Name:");
  const [first] = glyphs;
  assert.deepEqual(first.origin, [100, 700]);
  assert.equal(first.font, "Helvetica");
  assert.equal(first.size, 12);
  assert.deepEqual(first.direction, [1, 0]);
  assert.equal(first.bbox.length, 4);
  assert.equal(first.annotation, false);
  assert.equal(first.actualText, null);
  assert.throws(() => doc.glyphs(1), RangeError);
  assert.throws(() => doc.drawings(1), RangeError);
});

test("pages turn in place, the outline kept", () => {
  const outlined = build([
    "<< /Type /Catalog /Pages 2 0 R /Outlines 5 0 R >>",
    "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 /MediaBox [0 0 600 800] >>",
    "<< /Type /Page /Parent 2 0 R >>",
    "<< /Type /Page /Parent 2 0 R >>",
    "<< /Type /Outlines /First 6 0 R /Last 6 0 R /Count 1 >>",
    "<< /Title (Chapter) /Parent 5 0 R /Dest [3 0 R /Fit] >>",
  ]);
  const doc = Document.fromBytes(outlined);
  doc.rotatePages([[1, 1], [1, 1], [0, -1]]);
  assert.deepEqual(doc.geometry().map((page) => page.rotation), [270, 180]);
  const saved = Document.fromBytes(doc.save());
  assert.deepEqual(saved.geometry().map((page) => page.rotation), [270, 180]);
  assert.equal(saved.outline()[0].title, "Chapter");
  assert.throws(() => doc.rotatePages([[2, 1]]), RangeError);
});

test("links say where they lead, and pages their labels", () => {
  const doc = Document.fromBytes(build([
    "<< /Type /Catalog /Pages 2 0 R /PageLabels << /Nums [0 << /S /r >> 1 << /S /D /P (A-) >>] >> >>",
    "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>",
    "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] /Annots [5 0 R 6 0 R] >>",
    "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] >>",
    "<< /Type /Annot /Subtype /Link /Rect [10 20 110 40] /Dest [4 0 R /Fit] >>",
    "<< /Type /Annot /Subtype /Link /Rect [10 50 110 70] /A << /S /URI /URI (https://example.org) >> >>",
  ]));
  assert.deepEqual(doc.links(0), [
    { rect: [10, 20, 110, 40], kind: "page", pageNumber: 2 },
    { rect: [10, 50, 110, 70], kind: "uri", uri: "https://example.org" },
  ]);
  assert.deepEqual(doc.links(1), []);
  assert.deepEqual(doc.pageLabels(), ["i", "A-1"]);
  assert.throws(() => doc.links(2), RangeError);
});

test("an encrypted document is refused when asked, its sources too", () => {
  const protectedPdf = new Uint8Array(
    readFileSync(new URL("../../sealwax-core/tests/data/owner-protected.pdf", import.meta.url)),
  );
  // Opened by default, its user password empty.
  assert.equal(Document.fromBytes(protectedPdf).pageCount(), 1);
  const refused = (error) => error instanceof sealwax.PdfEncryptedError && error instanceof PdfReadError;
  assert.throws(() => Document.fromBytes(protectedPdf, { refuseEncrypted: true }), refused);
  assert.throws(() => mergeBytes([protectedPdf], { refuseEncrypted: true }), refused);
  const plain = Document.fromBytes(Document.fromBytes(protectedPdf).save());
  assert.throws(() => plain.insertPages(0, [protectedPdf], { refuseEncrypted: true }), refused);
  assert.equal(Document.fromBytes(plain.insertPages(0, [protectedPdf])).pageCount(), 2);
});

/** The XFDF a signing page exports, its `<add>` holding `marks`. */
function exported(marks) {
  return (
    '<?xml version="1.0" encoding="UTF-8" ?><xfdf xmlns="http://ns.adobe.com/xfdf/" xml:space="preserve">' +
    `<fields /><add>${marks.join("")}</add><modify /><delete /></xfdf>`
  );
}

test("a signing page's XFDF is applied, one per field, and names the pages to render again", () => {
  // A black picture: a black page rendered small.
  const black = Document.fromBytes(
    build([
      "<< /Type /Catalog /Pages 2 0 R >>",
      "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
      "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 60 20] /Contents 4 0 R >>",
      stream("0 0 0 rg 0 0 60 20 re f"),
    ]),
  ).renderPng(0, 1);
  const picture = `data:image/png;base64,${Buffer.from(black).toString("base64")}`;
  const stamp = (page, rect) =>
    `<stamp page="${page}" rect="${rect}" flags="print,readonly,locked" name="s${page}"><imagedata>${picture}</imagedata></stamp>`;
  const text =
    '<freetext page="0" rect="100,600,300,630" flags="print,readonly,locked" width="0" TextColor="#000000" FontSize="16">' +
    "<contents>Jean Été</contents><defaultappearance>0 0 0 rg /Helvetica 16 Tf</defaultappearance></freetext>";
  const doc = Document.fromBytes(
    build([
      "<< /Type /Catalog /Pages 2 0 R >>",
      "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>",
      "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>",
      "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Rotate 90 >>",
    ]),
  );
  const blank = doc.renderRgba(1, 0.5).pixels;
  const first = doc.applyXfdf(exported([text]));
  assert.deepEqual(first.pages, [0]);
  assert.equal(first.texts, 1);
  assert.equal(first.charactersReplaced, 0);
  assert.notEqual(first.flattened, null);
  const second = doc.applyXfdf(exported([stamp(1, "100,100,160,120")]), { flatten: false });
  assert.deepEqual(second.pages, [1]);
  assert.equal(second.pictures, 1);
  assert.equal(second.flattened, null);
  // Page 1, turned, now shows the picture.
  const drawn = doc.renderRgba(1, 0.5).pixels;
  assert.equal(drawn.length, blank.length);
  assert.ok(drawn.some((value, index) => value !== blank[index]));
  assert.equal(Document.fromBytes(doc.save()).pageCount(), 2);
});

test("applying an XFDF can take the document's own annotations out, and a refused one changes nothing", () => {
  const doc = Document.fromBytes(form());
  const applied = doc.applyXfdf(exported([]), { removeAnnotations: true });
  assert.equal(applied.annotationsRemoved, 2);
  assert.deepEqual(applied.pages, [0]);
  assert.equal(doc.fields().length, 0);
  const untouched = Document.fromBytes(form());
  assert.throws(
    () => untouched.applyXfdf(exported(['<ink page="0" rect="1,1,2,2" />']), { removeAnnotations: true }),
    (error) => error instanceof sealwax.XfdfError && error instanceof PdfError,
  );
  assert.equal(untouched.isModified(), false);
  assert.equal(untouched.fields().length, 2);
});

test("a part of a page renders alone, within the page", () => {
  const doc = Document.fromBytes(form());
  const whole = doc.renderRgba(0, 2, { quarterTurns: 1 });
  const part = doc.renderRgba(0, 2, { quarterTurns: 1, clip: { x: 300, y: 100, width: 50, height: 40 } });
  assert.equal(part.width, 50);
  assert.equal(part.height, 40);
  let largest = 0;
  for (let row = 0; row < 40; row++) {
    for (let i = 0; i < 50 * 4; i++) {
      const a = whole.pixels[((100 + row) * whole.width + 300) * 4 + i];
      largest = Math.max(largest, Math.abs(a - part.pixels[row * 50 * 4 + i]));
    }
  }
  assert.ok(largest <= 2, `${largest}`);
  assert.throws(
    () => doc.renderRgba(0, 2, { clip: { x: whole.height, y: 0, width: 1, height: 1 } }),
    RangeError,
  );
  assert.throws(() => doc.renderPng(0, 2, { clip: { x: 0, y: 0, width: 1.5, height: 1 } }), RangeError);
});

test("the XFDF of a signed field is written as applyXfdf reads it", () => {
  const png = Document.fromBytes(
    build([
      "<< /Type /Catalog /Pages 2 0 R >>",
      "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
      "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 60 20] /Contents 4 0 R >>",
      stream("0 0 0 rg 0 0 60 20 re f"),
    ]),
  ).renderPng(0, 1);
  const marks = [
    { kind: "stamp", pageIndex: 0, rect: [100, 560, 120, 620], rotation: 90, name: "a1", png, customData: { fieldId: "f-1" } },
    { kind: "freetext", pageIndex: 0, rect: [300, 400, 325, 520], rotation: 90, name: "t1", text: "Łukasz", align: "center" },
  ];
  const xfdf = sealwax.xfdfFor(marks, { deleted: [[0, "placeholder-1"]] });
  assert.equal(xfdf.split("<add>").length, 2);
  const doc = Document.fromBytes(
    build([
      "<< /Type /Catalog /Pages 2 0 R >>",
      "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
      "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Rotate 90 >>",
    ]),
  );
  const applied = doc.applyXfdf(xfdf);
  assert.deepEqual([applied.pictures, applied.texts, applied.deletionsIgnored], [1, 1, 1]);
  assert.throws(() => sealwax.xfdfFor([{ ...marks[0], rotation: 45 }]), sealwax.XfdfError);
  assert.throws(() => sealwax.xfdfFor([{ ...marks[0], png: new Uint8Array([1, 2, 3]) }]), sealwax.XfdfError);
  assert.throws(() => sealwax.xfdfFor([{ kind: "stamp", pageIndex: 0 }]), RangeError);
});

test("a font that is no TrueType program is refused before anything changes", () => {
  const xfdf = sealwax.xfdfFor([
    { kind: "freetext", pageIndex: 0, rect: [100, 600, 300, 630], name: "t1", text: "Łukasz", font: "Caveat" },
  ]);
  const doc = Document.fromBytes(form());
  assert.throws(() => doc.applyXfdf(xfdf, { fonts: { Caveat: new Uint8Array([1, 2, 3]) } }), RangeError);
  assert.throws(() => doc.applyXfdf(xfdf, { defaultFont: "Arial" }), RangeError);
  assert.equal(doc.isModified(), false);
  assert.equal(doc.applyXfdf(xfdf).charactersReplaced, 1);
});

test("a drawn signature is written and drawn", () => {
  const xfdf = sealwax.xfdfFor([
    { kind: "ink", pageIndex: 0, name: "i1", strokes: [[[100, 600], [120, 630], [150, 615]], [[160, 605]]], width: 1.5 },
  ]);
  const doc = Document.fromBytes(form());
  const applied = doc.applyXfdf(xfdf, { flatten: false });
  assert.deepEqual([applied.inks, applied.pictures, applied.texts], [1, 0, 0]);
  assert.deepEqual(applied.pages, [0]);
  assert.throws(() => sealwax.xfdfFor([{ kind: "ink", pageIndex: 0, name: "i1", strokes: [[[1, 2, 3]]] }]), RangeError);
  assert.throws(() => sealwax.xfdfFor([{ kind: "ink", pageIndex: 0, name: "i1", strokes: [[]] }]), sealwax.XfdfError);
});

// Last: a panic stops the module, and reset() makes another.
test("after a panic every call throws until reset(), and older documents stay refused", async () => {
  const bytes = build([
    "<< /Type /Catalog /Pages 2 0 R >>",
    "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
    "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] >>",
  ]);
  const before = Document.fromBytes(bytes);
  assert.equal(sealwax.isCrashed(), false);
  assert.throws(() => sealwax.__panicForTests(), sealwax.SealwaxCrashedError);
  assert.equal(sealwax.isCrashed(), true);
  assert.throws(() => before.pageCount(), sealwax.SealwaxCrashedError);
  assert.throws(() => Document.fromBytes(bytes), sealwax.SealwaxCrashedError);
  await sealwax.reset({ module_or_path: readFileSync(new URL("sealwax_bg.wasm", pkg)) });
  assert.equal(sealwax.isCrashed(), false);
  assert.equal(Document.fromBytes(bytes).pageCount(), 1);
  // A document of the stopped module means nothing in the new one.
  assert.throws(() => before.pageCount(), sealwax.SealwaxCrashedError);
  assert.throws(() => before.free(), sealwax.SealwaxCrashedError);
});
