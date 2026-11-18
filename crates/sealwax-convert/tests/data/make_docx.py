"""Writes the DOCX document the tests convert: written here, no third-party content.

    python make_docx.py

two-pages.docx: a paragraph, a link to https://example.org/, then a page break and a second
paragraph. No styles and no fonts: the layout falls back on its defaults. Entries are dated
1980-01-01, so that the file is the same at every run.
"""

import zipfile

W = "http://schemas.openxmlformats.org/wordprocessingml/2006/main"
R = "http://schemas.openxmlformats.org/officeDocument/2006/relationships"
PACKAGE = "http://schemas.openxmlformats.org/package/2006"
DOCUMENT_TYPE = "application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"

PARTS = {
    "[Content_Types].xml": f"""<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="{PACKAGE}/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="xml" ContentType="application/xml"/>
<Override PartName="/word/document.xml" ContentType="{DOCUMENT_TYPE}"/>
</Types>""",
    "_rels/.rels": f"""<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="{PACKAGE}/relationships">
<Relationship Id="rId1" Type="{R}/officeDocument" Target="word/document.xml"/>
</Relationships>""",
    "word/_rels/document.xml.rels": f"""<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="{PACKAGE}/relationships">
<Relationship Id="rId2" Type="{R}/hyperlink" Target="https://example.org/" TargetMode="External"/>
</Relationships>""",
    "word/document.xml": f"""<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="{W}" xmlns:r="{R}">
<w:body>
<w:p><w:r><w:t>Hello from a synthetic document.</w:t></w:r></w:p>
<w:p><w:hyperlink r:id="rId2"><w:r><w:t>A link</w:t></w:r></w:hyperlink></w:p>
<w:p><w:r><w:br w:type="page"/></w:r><w:r><w:t>Second page.</w:t></w:r></w:p>
<w:sectPr>
<w:pgSz w:w="11906" w:h="16838"/>
<w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440"/>
</w:sectPr>
</w:body>
</w:document>""",
}

with zipfile.ZipFile("two-pages.docx", "w", zipfile.ZIP_DEFLATED) as docx:
    for name, text in PARTS.items():
        docx.writestr(zipfile.ZipInfo(name, (1980, 1, 1, 0, 0, 0)), text, zipfile.ZIP_DEFLATED)
