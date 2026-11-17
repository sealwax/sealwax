# sealwax for the browser

sealwax is a PDF library written in Rust. This package is sealwax compiled to WebAssembly, for
browsers, web views and workers: read and fill forms, flatten, render pages, merge, extract pages,
save. It works on bytes in memory and never reaches the network.

```js
import init, { Document, PdfError } from "sealwax";
import wasmUrl from "sealwax/sealwax_bg.wasm?url"; // Vite; serve the file from your own origin

await init({ module_or_path: wasmUrl });

const doc = Document.fromBytes(new Uint8Array(await file.arrayBuffer()), {
  limits: { timeoutMs: 10_000 },
});
console.log(doc.pageCount(), doc.geometry());
doc.fillFields({ name: "Jane", agree: "Yes" });
const pdf = doc.save(); // Uint8Array
```

`init` takes the URL of `sealwax_bg.wasm`, its bytes, or a compiled `WebAssembly.Module`; nothing
is fetched from elsewhere. It works the same in a Web Worker, where long operations keep the page
responsive.

## Errors

Every failure is an instance of `PdfError`, or of a class under it: `PdfReadError` (not a PDF that
can be read), `PdfPasswordError`, `PdfLimitError` (its `limit` names the bound passed),
`PdfTimeoutError` (the deadline), `XfdfError`. An argument out of range is a `RangeError`. No
message quotes a document's content.

## Limits

Every operation runs within bounds (operators, image pixels, decoded bytes, annotations, rendered
pixels) and an optional deadline, `timeoutMs`, counted from the call that opens the document.
They are the first line of defence against hostile files, not a sandbox: run untrusted documents
in a worker you can terminate.

A panic cannot unwind in WebAssembly: where the native library would recover from a page it
cannot interpret, the module traps instead. Drop the instance (terminate the worker) and create
another.

## Licence

MIT OR Apache-2.0. `THIRD_PARTY_NOTICES.md` and `licenses/` list the code and data of others
compiled into the module.
