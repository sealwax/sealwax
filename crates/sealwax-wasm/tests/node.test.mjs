// The package under Node, through its `node` export: ready once imported, no fetch of a file: URL.
//
//   crates/sealwax-wasm/build.sh && node --test crates/sealwax-wasm/tests/node.test.mjs

import assert from "node:assert/strict";
import { test } from "node:test";

const sealwax = await import(new URL("../pkg/node.js", import.meta.url));

test("the node entry is ready once imported, and resets on its own", async () => {
  const pdf = new TextEncoder().encode(
    "%PDF-1.7\n1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n" +
      "2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n" +
      "3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] >>\nendobj\n" +
      "trailer\n<< /Root 1 0 R >>\n%%EOF\n",
  );
  assert.equal(sealwax.Document.fromBytes(pdf).pageCount(), 1);
  // Initializing again changes nothing; reset() reads the module from the package.
  await sealwax.default();
  await sealwax.reset();
  assert.equal(sealwax.Document.fromBytes(pdf).pageCount(), 1);
});
