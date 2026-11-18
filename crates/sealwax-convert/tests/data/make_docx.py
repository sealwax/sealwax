"""Writes the DOCX document the tests convert: written here, no third-party content.

    python make_docx.py

two-pages.docx: a heading, a paragraph, a link to https://example.org/, a list of two bullet
items, then a page break and a second paragraph. One style (the heading's, at outline level 1)
and one numbering (bullets); no fonts: the layout falls back on its defaults. Entries are dated
1980-01-01, so that the file is the same at every run.
"""

import zipfile

W = "http://schemas.openxmlformats.org/wordprocessingml/2006/main"
R = "http://schemas.openxmlformats.org/officeDocument/2006/relationships"
PACKAGE = "http://schemas.openxmlformats.org/package/2006"
WORDML = "application/vnd.openxmlformats-officedocument.wordprocessingml"
DOCUMENT_TYPE = f"{WORDML}.document.main+xml"

PARTS = {
    "[Content_Types].xml": f"""<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="{PACKAGE}/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="xml" ContentType="application/xml"/>
<Override PartName="/word/document.xml" ContentType="{DOCUMENT_TYPE}"/>
<Override PartName="/word/styles.xml" ContentType="{WORDML}.styles+xml"/>
<Override PartName="/word/numbering.xml" ContentType="{WORDML}.numbering+xml"/>
</Types>""",
    "_rels/.rels": f"""<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="{PACKAGE}/relationships">
<Relationship Id="rId1" Type="{R}/officeDocument" Target="word/document.xml"/>
</Relationships>""",
    "word/_rels/document.xml.rels": f"""<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="{PACKAGE}/relationships">
<Relationship Id="rId2" Type="{R}/hyperlink" Target="https://example.org/" TargetMode="External"/>
<Relationship Id="rId3" Type="{R}/styles" Target="styles.xml"/>
<Relationship Id="rId4" Type="{R}/numbering" Target="numbering.xml"/>
</Relationships>""",
    "word/styles.xml": f"""<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:styles xmlns:w="{W}">
<w:style w:type="paragraph" w:styleId="Heading1">
<w:name w:val="heading 1"/>
<w:pPr><w:outlineLvl w:val="0"/></w:pPr>
<w:rPr><w:b/><w:sz w:val="32"/></w:rPr>
</w:style>
</w:styles>""",
    "word/numbering.xml": f"""<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:numbering xmlns:w="{W}">
<w:abstractNum w:abstractNumId="0">
<w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="bullet"/><w:lvlText w:val="•"/>
<w:pPr><w:ind w:left="720" w:hanging="360"/></w:pPr></w:lvl>
</w:abstractNum>
<w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num>
</w:numbering>""",
    "word/document.xml": f"""<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="{W}" xmlns:r="{R}">
<w:body>
<w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>A heading</w:t></w:r></w:p>
<w:p><w:r><w:t>Hello from a synthetic document.</w:t></w:r></w:p>
<w:p><w:hyperlink r:id="rId2"><w:r><w:t>A link</w:t></w:r></w:hyperlink></w:p>
<w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr></w:pPr><w:r><w:t>First item</w:t></w:r></w:p>
<w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr></w:pPr><w:r><w:t>Second item</w:t></w:r></w:p>
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
