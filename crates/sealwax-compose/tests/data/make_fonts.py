"""Writes the font programs the tests embed: boxes drawn here, no third-party outlines.

    python make_fonts.py   (needs fontTools)

boxes-cff.otf: CFF outlines; boxes.ttc: a collection of a TrueType face (0) and that CFF face (1).
Glyphs: .notdef, H, i, exclam, boxes of distinct advances (500, 600, 300, 400 per 1000 units).
"""

from fontTools.fontBuilder import FontBuilder
from fontTools.pens.t2CharStringPen import T2CharStringPen
from fontTools.pens.ttGlyphPen import TTGlyphPen
from fontTools.ttLib import TTCollection

GLYPHS = [(".notdef", None, 500), ("H", "H", 600), ("i", "i", 300), ("exclam", "!", 400)]


def box(pen, advance):
    pen.moveTo((50, 0))
    pen.lineTo((advance - 50, 0))
    pen.lineTo((advance - 50, 700))
    pen.lineTo((50, 700))
    pen.closePath()


def builder(cff):
    fb = FontBuilder(1000, isTTF=not cff)
    fb.setupGlyphOrder([name for name, _, _ in GLYPHS])
    fb.setupCharacterMap({ord(c): name for name, c, _ in GLYPHS if c})
    fb.setupHorizontalMetrics({name: (advance, 50) for name, _, advance in GLYPHS})
    fb.setupHorizontalHeader(ascent=800, descent=-200)
    names = {"familyName": "Boxes", "styleName": "Regular"}
    if cff:
        charstrings = {}
        for name, _, advance in GLYPHS:
            pen = T2CharStringPen(advance, None)
            box(pen, advance)
            charstrings[name] = pen.getCharString()
        fb.setupCFF("BoxesCFF-Regular", {"FullName": "Boxes CFF"}, charstrings, {})
        names["psName"] = "BoxesCFF-Regular"
    else:
        glyphs = {}
        for name, _, advance in GLYPHS:
            pen = TTGlyphPen(None)
            box(pen, advance)
            glyphs[name] = pen.glyph()
        fb.setupGlyf(glyphs)
        names["psName"] = "BoxesTT-Regular"
    fb.setupNameTable(names)
    fb.setupOS2(sTypoAscender=800, sTypoDescender=-200, sCapHeight=700)
    fb.setupPost()
    return fb.font


cff = builder(True)
cff.save("boxes-cff.otf")
collection = TTCollection()
collection.fonts = [builder(False), cff]
collection.save("boxes.ttc")
