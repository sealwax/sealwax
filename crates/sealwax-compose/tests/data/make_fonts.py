"""Writes the font programs the tests embed: boxes drawn here, no third-party outlines.

    python make_fonts.py   (needs fontTools)

boxes-cff.otf: CFF outlines; boxes.ttc: a collection of a TrueType face (0) and that CFF face (1).
Glyphs: .notdef, H, i, exclam, boxes of distinct advances (500, 600, 300, 400 per 1000 units).

font-dicts.otf: a CID-keyed CFF font whose 256 glyphs past .notdef, drawn by U+0100 to U+01FF,
each have their own font dictionary, which subsetter 0.2.6 panics on (typst/subsetter#34).
"""

from fontTools.cffLib import FDArrayIndex, FDSelect, FontDict, PrivateDict
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


def font_dicts():
    names = [".notdef"] + [f"cid{cid:05d}" for cid in range(1, 257)]
    fb = FontBuilder(1000, isTTF=False)
    fb.setupGlyphOrder(names)
    fb.setupCharacterMap({0x100 + cid - 1: names[cid] for cid in range(1, 257)})
    fb.setupHorizontalMetrics({name: (500, 50) for name in names})
    fb.setupHorizontalHeader(ascent=800, descent=-200)
    charstrings = {}
    for name in names:
        pen = T2CharStringPen(500, None)
        box(pen, 500)
        charstrings[name] = pen.getCharString()
    fb.setupCFF("FontDicts-Regular", {"FullName": "Font Dicts"}, charstrings, {})
    fb.setupNameTable(
        {"familyName": "Font Dicts", "styleName": "Regular", "psName": "FontDicts-Regular"}
    )
    fb.setupOS2(sTypoAscender=800, sTypoDescender=-200, sCapHeight=700)
    fb.setupPost()
    cff = fb.font["CFF "].cff
    top = cff.topDictIndex[0]
    top.ROS = ("Adobe", "Identity", 0)
    top.CIDCount = len(names)
    fd_array = FDArrayIndex()
    fd_array.strings = None
    fd_array.GlobalSubrs = cff.GlobalSubrs
    for _ in range(256):
        font_dict = FontDict()
        font_dict.Private = PrivateDict()
        fd_array.append(font_dict)
    top.FDArray = fd_array
    top.FDSelect = FDSelect()
    top.FDSelect.format = 0
    top.FDSelect.gidArray = [0] + list(range(256))
    del top.Private
    top.rawDict.pop("Private", None)
    for cid, name in enumerate(names):
        top.CharStrings[name].private = fd_array[max(cid - 1, 0)].Private
    return fb.font


cff = builder(True)
cff.save("boxes-cff.otf")
collection = TTCollection()
collection.fonts = [builder(False), cff]
collection.save("boxes.ttc")
font_dicts().save("font-dicts.otf")
