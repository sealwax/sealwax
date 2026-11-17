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
  assert.deepEqual(filled, { occurrences: 2, charactersReplaced: 0, unmatched: [] });
  assert.ok(doc.isModified());
  const reopened = Document.fromBytes(doc.save());
  assert.deepEqual(reopened.fieldValues(), { name: ["Élodie"], agree: ["Yes"] });
  assert.deepEqual(reopened.fillFields({ agree: "Maybe" }).unmatched, ["agree"]);
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
