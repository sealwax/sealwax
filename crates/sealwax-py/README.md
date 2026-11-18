# sealwax for Python

Python bindings of [sealwax](https://github.com/sealwax/sealwax), a PDF library written in Rust.

- Read PDF documents and their page geometry.
- Fill and flatten annotations, apply XFDF.
- Convert to PDF/A-3 (levels b, u and a).
- Sign with PAdES signatures, timestamps, long-term validation data and document timestamps
  (up to B-LTA). The key stays in your HSM: any object with `sign_digest` signs.
- Check the integrity of the signatures a document holds.
- Convert Word documents (DOCX) to PDF, tagged with their structure and bookmarks, laid out by
  [docxide-pdf](https://github.com/sverrejb/docxide-pdf). Fonts are those installed, and the
  directories you name first.

sealwax works with the interpreter lock released, within `Limits` that bound what a hostile document
may cost, and can run an operation in a process of its own, with a memory cap
(`sealwax.isolated`).

```python
import sealwax

data = open("in.pdf", "rb").read()
archived, report = sealwax.pdfa_convert(data, level="3a", limits=sealwax.Limits(timeout=60))

# The layout's own time is not bounded: a document you do not trust converts in a process of
# its own.
docx = open("in.docx", "rb").read()
pdf, report = sealwax.isolated(sealwax.docx_to_pdf, docx, timeout=60, memory=2 << 30)
```

Licensed under MIT OR Apache-2.0, at your option.
