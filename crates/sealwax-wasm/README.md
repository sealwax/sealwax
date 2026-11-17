# sealwax for the browser

sealwax is a PDF library written in Rust. This package is sealwax compiled to WebAssembly, for
browsers, web views, workers and Node: read and fill forms, flatten, render pages, merge, extract
and turn pages, redact, save. It works on bytes in memory and never reaches the network.

```js
import init, { Document, PdfError } from "sealwax";
import wasmUrl from "sealwax/sealwax_bg.wasm?url"; // Vite; serve the file from your own origin

await init({ module_or_path: wasmUrl });

const doc = Document.fromBytes(new Uint8Array(await file.arrayBuffer()), {
  limits: { timeoutMs: 10_000 },
});
console.log(doc.pageCount(), doc.geometry());
doc.fillFields({ name: "Jane", agree: "Yes" });
doc.rotatePages([[0, 1]]); // a quarter turn clockwise, in place
const pdf = doc.save(); // Uint8Array
```

`init` takes the URL of `sealwax_bg.wasm`, its bytes, or a compiled `WebAssembly.Module`; nothing
is fetched from elsewhere. It works the same in a Web Worker, where long operations keep the page
responsive.

The module uses WebAssembly SIMD: Chrome and Edge 91, Firefox 89, Safari 16.4 (iOS 16.4) and
Node 16.4 or later. An older engine refuses to compile it (`WebAssembly.CompileError` from
`init`).

## Node

Under Node, the package's `node` export reads `sealwax_bg.wasm` from the package itself and is
ready once imported: no `init` to call (calling it changes nothing). Node's `fetch` does not read
`file:` URLs, which is what the browser entry would do.

```js
import { Document, mergeBytes } from "sealwax";
```

## Errors

Every failure about a document is an instance of `PdfError`, or of a class under it:
`PdfReadError` (not a PDF that can be read), `PdfEncryptedError` (under `PdfReadError`: an
encrypted document refused), `PdfPasswordError`, `PdfLimitError` (its `limit` names the bound
passed), `PdfTimeoutError` (the deadline), `XfdfError`, `FieldNameTakenError`. An argument out of
range is a `RangeError`. No message quotes a document's content.

## Encrypted documents

A document protected by an owner password only (its user password empty) opens without one, as in
every viewer, and sealwax saves documents unencrypted: a copy of it loses its protection. To refuse
encrypted documents instead, pass `refuseEncrypted: true` to `Document.fromBytes`, `mergeBytes` or
`insertPages`: they throw `PdfEncryptedError`.

## Pages

`rotatePages` turns pages in place: the document keeps everything it has (outline, structure,
information). `arrange`, `extractPages`, `insertPages` and `mergeBytes` make a new document of
the pages, their form kept; `arrange` refuses a page listed twice.

## Signed fields (XFDF)

`applyXfdf` draws into the pages the XFDF a signing page exports (its `<add>`: PNG stamps and
free texts, turned by quarter turns when a page is), with the code a server running sealwax uses
for the final PDF: what shows on screen is what the final PDF holds. The document's own
annotations are flattened first (`flatten`, true unless said otherwise), or taken out
(`removeAnnotations`); apply one XFDF per signed field, one after the other. The result's `pages`
lists the pages to render again.

```js
for (const xfdf of storedXfdfs) {
  const { pages } = doc.applyXfdf(xfdf);
  pages.forEach(renderPage);
}
```

`xfdfFor` writes the XFDF of the fields a signer just signed (PNG stamps and free texts, in PDF
user space, `rotation` the page's own on a turned page), exactly what `applyXfdf` reads: send it
to the server, apply it here to show it.

```js
const xfdf = xfdfFor([{ kind: "stamp", pageIndex, rect, rotation, name: id, png, customData: { fieldId } }]);
doc.applyXfdf(xfdf);
```

## Zoom

A page shown larger than the screen renders by the part that shows: `clip` takes a rectangle in
pixels of the whole page's image at that scale (turned as asked), from its top-left corner, and
`maxRenderPixels` counts its pixels alone. Each call interprets the page whole and decodes the
images the part shows: render what the screen shows in one call (with a margin to scroll into)
rather than many small tiles, and cancel a part that leaves the screen with `cancel`.

```js
const view = doc.renderRgba(page, 3 * devicePixelRatio, {
  clip: { x: scrollLeft, y: scrollTop, width: canvas.width, height: canvas.height },
});
```

## Limits and panics

Every operation runs within bounds (operators, image pixels, decoded bytes, annotations, rendered
pixels) and an optional deadline, `timeoutMs`, counted from the call that opens the document.
They are the first line of defence against hostile files, not a sandbox.

A panic cannot unwind in WebAssembly: where the native library would recover from a page it
cannot interpret, the module stops. Every call then throws `SealwaxCrashedError` (not a
`PdfError`: the document is not the cause it names) and `isCrashed()` says so, until `reset()`
makes the module anew (`await reset(source)`, `source` as `init` takes it; under Node, none). The
documents opened before are lost: using one throws `SealwaxCrashedError`; open them again. To keep
a page's other work going while a document is processed, run sealwax in a worker you can
terminate.

## TypeScript

The declarations name `Symbol.dispose` (documents can be used with `using`): compile with the
`esnext.disposable` library (`"lib": ["es2022", "esnext.disposable", "dom"]`), or a later one.

## Licence

MIT OR Apache-2.0. `THIRD_PARTY_NOTICES.md` and `licenses/` list the code and data of others
compiled into the module.
