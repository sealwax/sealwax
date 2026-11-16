"""Synthetic hostile inputs for sealwax's robustness bench:
decompression bombs, cycles, exponential fan-outs, deep nesting, sizes read from the file, and
malicious XFDF. Nothing here comes from a real document; every file is built from scratch.

    python -I hostile.py OUT_DIR

writes `OUT_DIR/pdf/<case>.pdf`, `OUT_DIR/xfdf/<case>.xfdf` and `OUT_DIR/benign.pdf` (the page the
hostile XFDF are applied to), plus `OUT_DIR/cases.json`, what each case attacks.
"""

import base64
import json
import struct
import sys
import zlib
from pathlib import Path

GIB = 1 << 30
CHUNK = 1 << 20

CASES: dict[str, str] = {}
PDFS: dict[str, bytes] = {}
XFDFS: dict[str, bytes] = {}


def case(name: str, threat: str, data: bytes, kind: str = "pdf") -> None:
    CASES[f"{kind}/{name}"] = threat
    (PDFS if kind == "pdf" else XFDFS)[name] = data


# --- Writing PDF ---------------------------------------------------------------------------------

def deflate(data: bytes) -> bytes:
    return zlib.compress(data, 9)


def deflate_repeated(unit: bytes, total: int) -> bytes:
    """zlib stream of `unit` repeated up to `total` bytes, without holding them."""
    chunk = unit * max(1, CHUNK // len(unit))
    compressor = zlib.compressobj(9)
    out = bytearray()
    written = 0
    while written < total:
        piece = chunk[: total - written]
        out += compressor.compress(piece)
        written += len(piece)
    out += compressor.flush()
    return bytes(out)


def stream(entries: bytes, data: bytes) -> bytes:
    return b"<< " + entries + b" /Length %d >>\nstream\n" % len(data) + data + b"\nendstream"


def build(objects: dict[int, bytes], trailer: bytes = b"/Root 1 0 R", size: int | None = None) -> bytes:
    """A PDF with a classic cross-reference table."""
    out = bytearray(b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n")
    offsets = {}
    for number in sorted(objects):
        offsets[number] = len(out)
        out += b"%d 0 obj\n" % number + objects[number] + b"\nendobj\n"
    count = max(objects) + 1
    at = len(out)
    out += b"xref\n0 %d\n0000000000 65535 f \n" % count
    for number in range(1, count):
        out += b"%010d 00000 n \n" % offsets[number] if number in offsets else b"0000000000 65535 f \n"
    out += b"trailer\n<< /Size %d %s >>\nstartxref\n%d\n%%%%EOF\n" % (size or count, trailer, at)
    return bytes(out)


def build_xref_stream(objects: dict[int, bytes], packed: dict[int, tuple[int, int]], pad: int = 0,
                      xref_entries: bytes = b"") -> bytes:
    """A PDF with a cross-reference stream; `packed` maps an object to (object stream, index).
    `pad` zeros follow the rows in the decoded stream."""
    out = bytearray(b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n")
    offsets = {}
    for number in sorted(objects):
        offsets[number] = len(out)
        out += b"%d 0 obj\n" % number + objects[number] + b"\nendobj\n"
    xref_number = max([*objects, *packed]) + 1
    offsets[xref_number] = len(out)
    rows = bytearray(b"\x00" + struct.pack(">I", 0) + b"\xff\xff")
    for number in range(1, xref_number + 1):
        if number in offsets:
            rows += b"\x01" + struct.pack(">I", offsets[number]) + b"\x00\x00"
        elif number in packed:
            rows += b"\x02" + struct.pack(">I", packed[number][0]) + struct.pack(">H", packed[number][1])
        else:
            rows += b"\x00" + struct.pack(">I", 0) + b"\xff\xff"
    data = _concat_deflate(bytes(rows), b"\x00", pad) if pad else deflate(bytes(rows))
    out += b"%d 0 obj\n" % xref_number + stream(
        b"/Type /XRef /Size %d /W [1 4 2] /Root 1 0 R /Filter /FlateDecode " % (xref_number + 1) + xref_entries,
        data,
    ) + b"\nendobj\n"
    out += b"startxref\n%d\n%%%%EOF\n" % offsets[xref_number]
    return bytes(out)


FONT = b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>"
TEXT = b"BT /F1 24 Tf 72 700 Td (sealwax hostile bench) Tj ET"


def page_objects(content: bytes, resources: bytes = b"/Font << /F1 5 0 R >>", page_extra: bytes = b"",
                 content_entries: bytes = b"") -> dict[int, bytes]:
    """Catalog 1, pages 2, page 3, content 4, font 5."""
    return {
        1: b"<< /Type /Catalog /Pages 2 0 R >>",
        2: b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        3: b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << " + resources
           + b" >> /Contents 4 0 R " + page_extra + b">>",
        4: stream(content_entries, content),
        5: FONT,
    }


def simple(content: bytes = TEXT, **kwargs) -> bytes:
    return build(page_objects(content, **kwargs))


def png(width: int, height: int, idat: bytes, color_type: int = 0) -> bytes:
    def chunk(kind: bytes, body: bytes) -> bytes:
        return struct.pack(">I", len(body)) + kind + body + struct.pack(">I", zlib.crc32(kind + body))
    header = struct.pack(">IIBBBBB", width, height, 8, color_type, 0, 0, 0)
    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", header) + chunk(b"IDAT", idat) + chunk(b"IEND", b"")


# --- Decompression bombs -------------------------------------------------------------------------

def bombs() -> None:
    case("bomb-content", "page content: the text, then 1 GiB of spaces, Flate (1000:1)",
         _with_content(_concat_deflate(TEXT + b"\n", b" ", GIB), b"/Filter /FlateDecode"))
    case("bomb-content-nested", "page content: 4 GiB of spaces, Flate applied twice",
         _with_content(deflate(deflate_repeated(b" ", 4 * GIB)), b"/Filter [/FlateDecode /FlateDecode]"))
    case("bomb-operators", "page content: 256 MiB of operators, 55 million",
         _with_content(deflate_repeated(b"0 0 m 1 1 l S\n", 256 << 20), b"/Filter /FlateDecode"))
    case("path-huge", "one path of 33 million segments, filled",
         _with_content(_concat_deflate(b"0 0 m\n", b"612 792 l 0 0 l\n", 256 << 20, b"f\n"), b"/Filter /FlateDecode"))
    case("dash-tiny", "a long line stroked with a dash of 1e-5 pt",
         simple(b"[0.00001] 0 d 0 0 m 612 792 l 0 792 l 612 0 l S"))
    case("bomb-lzw", "page content: 64 MiB of spaces, LZW (1300:1)",
         _with_content(_lzw_spaces(64 << 20), b"/Filter /LZWDecode"))
    hexed = deflate_repeated(b" ", GIB).hex().encode() + b">"
    case("bomb-hex-flate", "page content: 1 GiB of spaces, Flate then ASCIIHex",
         _with_content(hexed, b"/Filter [/ASCIIHexDecode /FlateDecode]"))
    case("bomb-middle-stage", "page content: Flate to 1 GiB of spaces that ASCIIHex reads as nothing",
         _with_content(deflate_repeated(b" ", GIB), b"/Filter [/FlateDecode /ASCIIHexDecode]"))

    # Object stream: the page dictionary, then 1 GiB of spaces.
    header = b"3 0 "
    page = (b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R "
            b"/Resources << /Font << /F1 5 0 R >> >> >>")
    objects = page_objects(TEXT)
    del objects[3]
    objects[6] = stream(b"/Type /ObjStm /N 1 /First %d /Filter /FlateDecode" % len(header),
                        _concat_deflate(header + page, b" ", GIB))
    case("bomb-objstm", "object stream holding the page: 1 GiB decoded", build_xref_stream(objects, {3: (6, 0)}))
    case("bomb-xrefstm", "cross-reference stream: its rows, then 1 GiB of zeros",
         build_xref_stream(page_objects(TEXT), {}, pad=GIB))

    side = 32768
    objects = page_objects(b"q 612 0 0 792 0 0 cm /Im0 Do Q", resources=b"/XObject << /Im0 6 0 R >>")
    objects[6] = stream(b"/Type /XObject /Subtype /Image /Width %d /Height %d /ColorSpace /DeviceGray "
                        b"/BitsPerComponent 8 /Filter /FlateDecode" % (side, side),
                        deflate_repeated(b"\x80", side * side))
    case("bomb-image", "image 32768 x 32768 gray, 1 GiB decoded, drawn", build(objects))

    objects = page_objects(b"q 612 0 0 792 0 0 cm /Im0 Do Q", resources=b"/XObject << /Im0 6 0 R >>")
    objects[6] = stream(b"/Type /XObject /Subtype /Image /Width 100000 /Height 100000 /ColorSpace /DeviceRGB "
                        b"/BitsPerComponent 8 /Filter /FlateDecode", deflate(b"\x00" * 1024))
    case("image-dimensions", "image declared 100000 x 100000 RGB (30 GB), 1 KiB of data", build(objects))

    objects = page_objects(b"q 612 0 0 792 0 0 cm BI /W 100000 /H 100000 /CS /G /BPC 8 ID \x00\x00\x00 EI Q")
    case("inline-image-dimensions", "inline image declared 100000 x 100000", build(objects))

    objects = page_objects(b"q 612 0 0 792 0 0 cm /Im0 Do Q", resources=b"/XObject << /Im0 6 0 R >>")
    objects[6] = stream(b"/Type /XObject /Subtype /Image /Width 16 /Height 16 /ColorSpace /DeviceGray "
                        b"/BitsPerComponent 8 /Filter /FlateDecode /DecodeParms << /Predictor 12 "
                        b"/Columns 2000000000 /Colors 4 >>", deflate(b"\x00" * 64))
    case("predictor-columns", "PNG predictor with /Columns 2e9 and /Colors 4", build(objects))

    objects = page_objects(b"BT /F1 24 Tf 72 700 Td (hostile) Tj ET")
    objects[5] = (b"<< /Type /Font /Subtype /TrueType /BaseFont /Bomb /FirstChar 32 /LastChar 126 "
                  b"/Widths [" + b"500 " * 95 + b"] /FontDescriptor 6 0 R >>")
    objects[6] = (b"<< /Type /FontDescriptor /FontName /Bomb /Flags 32 /FontBBox [0 0 1000 1000] /ItalicAngle 0 "
                  b"/Ascent 800 /Descent -200 /CapHeight 700 /StemV 80 /FontFile2 7 0 R >>")
    objects[7] = stream(b"/Filter /FlateDecode", deflate_repeated(b"\x00", GIB))
    case("bomb-font", "embedded TrueType program: 1 GiB decoded", build(objects))

    objects = page_objects(TEXT)
    objects[5] = b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /ToUnicode 6 0 R >>"
    objects[6] = stream(b"/Filter /FlateDecode", deflate_repeated(b" ", GIB))
    case("bomb-tounicode", "ToUnicode CMap: 1 GiB decoded", build(objects))

    objects = page_objects(TEXT)
    objects[1] = b"<< /Type /Catalog /Pages 2 0 R /Metadata 6 0 R >>"
    objects[6] = stream(b"/Type /Metadata /Subtype /XML /Filter /FlateDecode", deflate_repeated(b" ", GIB))
    case("bomb-metadata", "XMP metadata: 1 GiB decoded", build(objects))

    objects = page_objects(TEXT)
    objects[1] = b"<< /Type /Catalog /Pages 2 0 R /Names << /EmbeddedFiles << /Names [(a.bin) 6 0 R] >> >> >>"
    objects[6] = b"<< /Type /Filespec /F (a.bin) /UF (a.bin) /EF << /F 7 0 R >> >>"
    objects[7] = stream(b"/Type /EmbeddedFile /Filter /FlateDecode", deflate_repeated(b"\x00", GIB))
    case("bomb-attachment", "embedded file: 1 GiB decoded", build(objects))

    objects = page_objects(TEXT, page_extra=b"/Annots [6 0 R] ")
    objects[6] = b"<< /Type /Annot /Subtype /Square /Rect [100 100 300 300] /F 4 /AP << /N 7 0 R >> >>"
    objects[7] = stream(b"/Type /XObject /Subtype /Form /BBox [0 0 200 200] /Filter /FlateDecode",
                        _concat_deflate(b"0 0 200 200 re f\n", b" ", GIB))
    case("bomb-appearance", "annotation appearance: 1 GiB decoded", build(objects))

    objects = page_objects(b"q 612 0 0 792 0 0 cm /Im0 Do Q", resources=b"/XObject << /Im0 6 0 R >>")
    objects[6] = stream(b"/Type /XObject /Subtype /Image /Width 16 /Height 16 /ColorSpace /DeviceGray "
                        b"/BitsPerComponent 8 /SMask 7 0 R /Filter /FlateDecode", deflate(b"\x00" * 256))
    objects[7] = stream(b"/Type /XObject /Subtype /Image /Width 32768 /Height 32768 /ColorSpace /DeviceGray "
                        b"/BitsPerComponent 8 /Filter /FlateDecode", deflate_repeated(b"\xff", GIB))
    case("bomb-smask", "soft mask of a small image: 1 GiB decoded", build(objects))


def _concat_deflate(head: bytes, unit: bytes, total: int, tail: bytes = b"") -> bytes:
    """zlib stream of `head`, `unit` repeated up to `total` bytes, then `tail`."""
    compressor = zlib.compressobj(9)
    out = bytearray(compressor.compress(head))
    chunk = unit * max(1, CHUNK // len(unit))
    written = 0
    while written < total:
        piece = chunk[: total - written]
        out += compressor.compress(piece)
        written += len(piece)
    out += compressor.compress(tail) + compressor.flush()
    return bytes(out)


def _with_content(data: bytes, entries: bytes) -> bytes:
    objects = page_objects(b"")
    objects[4] = stream(entries, data)
    return build(objects)


def _lzw_width(index: int) -> int:
    """Width of the code read `index`-th since the last clear, early change on (ISO 32000-1, 7.4.4)."""
    next_code = 258 + max(0, index - 1)
    return min(12, max(9, (next_code + 1).bit_length()))


def _lzw_spaces(total: int) -> bytes:
    """LZW of at least `total` spaces: after a clear, code 32 then 258, 259... each one byte longer."""
    bits = 0
    count = 0
    out = bytearray()

    def write(code: int, width: int) -> None:
        nonlocal bits, count
        bits = (bits << width) | code
        count += width
        while count >= 8:
            count -= 8
            out.append((bits >> count) & 0xFF)
        bits &= (1 << count) - 1

    write(256, 9)
    produced = 0
    while produced < total:
        codes = [32]
        produced += 1
        code = 258
        while code < 4000 and produced < total:
            codes.append(code)
            produced += code - 256
            code += 1
        for index, value in enumerate(codes):
            write(value, _lzw_width(index))
        write(256 if produced < total else 257, _lzw_width(len(codes)))
    if count:
        out.append((bits << (8 - count)) & 0xFF)
    return bytes(out)


# --- Graphs: cycles and exponential fan-outs -----------------------------------------------------

def graphs() -> None:
    objects = page_objects(TEXT)
    objects[2] = b"<< /Type /Pages /Kids [2 0 R 3 0 R] /Count 2 >>"
    case("pages-cycle", "page tree whose root lists itself", build(objects))

    objects = page_objects(TEXT)
    objects[2] = b"<< /Type /Pages /Kids [6 0 R] /Count 1 >>"
    objects[6] = b"<< /Type /Pages /Parent 2 0 R /Kids [2 0 R] /Count 1 >>"
    case("pages-cycle-2", "page tree: root -> node -> root", build(objects))

    objects = page_objects(TEXT)
    depth = 40
    objects[2] = b"<< /Type /Pages /Kids [10 0 R 10 0 R] /Count %d >>" % (1 << depth)
    for level in range(depth - 1):
        objects[10 + level] = b"<< /Type /Pages /Kids [%d 0 R %d 0 R] /Count %d >>" % (
            11 + level, 11 + level, 1 << (depth - 1 - level))
    objects[10 + depth - 1] = b"<< /Type /Pages /Kids [3 0 R 3 0 R] /Count 2 >>"
    case("pages-fanout", "page tree as a DAG: 2^40 leaves through 40 shared nodes", build(objects))

    objects = page_objects(TEXT)
    pages = 100_000
    objects[2] = b"<< /Type /Pages /Kids [" + b"".join(b"%d 0 R " % (10 + i) for i in range(pages)) \
        + b"] /Count %d >>" % pages
    for i in range(pages):
        objects[10 + i] = b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R " \
                          b"/Resources << /Font << /F1 5 0 R >> >> >>"
    del objects[3]
    case("pages-many", "100 000 pages sharing one content stream", build(objects))

    objects = page_objects(TEXT)
    objects[2] = b"<< /Type /Pages /Kids [3 0 R] /Count 2147483647 >>"
    case("pages-count-huge", "page tree declaring 2^31 - 1 pages, holding 1", build(objects))

    objects = page_objects(b"/X0 Do", resources=b"/XObject << /X0 6 0 R >>")
    objects[6] = stream(b"/Type /XObject /Subtype /Form /BBox [0 0 612 792] /Resources << /XObject << /X0 6 0 R >> >>",
                        b"0 0 10 10 re f /X0 Do")
    case("xobject-cycle", "form XObject drawing itself", build(objects))

    objects = page_objects(b"/X0 Do", resources=b"/XObject << /X0 10 0 R >>")
    depth = 10
    for level in range(depth):
        body = b"0 0 1 1 re f" if level == depth - 1 else b"/X Do " * 10
        objects[10 + level] = stream(
            b"/Type /XObject /Subtype /Form /BBox [0 0 612 792] /Resources << /XObject << /X %d 0 R >> >>"
            % (11 + min(level, depth - 2)), body)
    case("xobject-fanout", "form XObjects drawing the next ten times, 10 levels: 10^9 draws", build(objects))

    objects = page_objects(b"BT /T3 12 Tf 72 700 Td (a) Tj ET", resources=b"/Font << /T3 6 0 R >>")
    objects[6] = (b"<< /Type /Font /Subtype /Type3 /FontBBox [0 0 1000 1000] /FontMatrix [0.001 0 0 0.001 0 0] "
                  b"/CharProcs << /a 7 0 R >> /Encoding << /Differences [97 /a] >> /FirstChar 97 /LastChar 97 "
                  b"/Widths [1000] /Resources << /Font << /T3 6 0 R >> >> >>")
    objects[7] = stream(b"", b"1000 0 d0 BT /T3 12 Tf (a) Tj ET")
    case("type3-cycle", "Type 3 glyph drawing itself", build(objects))

    objects = page_objects(b"/Pattern cs /P0 scn 0 0 612 792 re f", resources=b"/Pattern << /P0 6 0 R >>")
    objects[6] = stream(b"/Type /Pattern /PatternType 1 /PaintType 1 /TilingType 1 /BBox [0 0 10 10] /XStep 10 "
                        b"/YStep 10 /Resources << /Pattern << /P0 6 0 R >> >>",
                        b"/Pattern cs /P0 scn 0 0 10 10 re f")
    case("pattern-cycle", "tiling pattern painting with itself", build(objects))

    objects = page_objects(b"/Pattern cs /P0 scn 0 0 612 792 re f", resources=b"/Pattern << /P0 6 0 R >>")
    objects[6] = stream(b"/Type /Pattern /PatternType 1 /PaintType 1 /TilingType 1 /BBox [0 0 0.001 0.001] "
                        b"/XStep 0.001 /YStep 0.001", b"0 0 0.001 0.001 re f")
    case("pattern-tiny-step", "tiling pattern with a step of 0.001 pt over a full page", build(objects))

    objects = page_objects(b"/GS0 gs 0 0 612 792 re f", resources=b"/ExtGState << /GS0 6 0 R >>")
    objects[6] = b"<< /Type /ExtGState /SMask << /S /Luminosity /G 7 0 R >> >>"
    objects[7] = stream(b"/Type /XObject /Subtype /Form /BBox [0 0 612 792] /Group << /S /Transparency >> "
                        b"/Resources << /ExtGState << /GS0 6 0 R >> >>", b"/GS0 gs 0 0 612 792 re f")
    case("softmask-cycle", "soft mask whose group uses the same soft mask", build(objects))

    objects = page_objects(TEXT)
    objects[1] = b"<< /Type /Catalog /Pages 2 0 R /Outlines 6 0 R >>"
    objects[6] = b"<< /Type /Outlines /First 7 0 R /Last 8 0 R /Count 2 >>"
    objects[7] = b"<< /Title (a) /Parent 6 0 R /Next 8 0 R /First 7 0 R >>"
    objects[8] = b"<< /Title (b) /Parent 6 0 R /Prev 7 0 R /Next 7 0 R >>"
    case("outline-cycle", "outline items: /Next loops, an item its own child", build(objects))

    objects = page_objects(TEXT, page_extra=b"/StructParents 0 ")
    objects[1] = b"<< /Type /Catalog /Pages 2 0 R /MarkInfo << /Marked true >> /StructTreeRoot 6 0 R >>"
    objects[6] = b"<< /Type /StructTreeRoot /K 7 0 R /ParentTree << /Nums [] >> >>"
    objects[7] = b"<< /Type /StructElem /S /Document /P 6 0 R /K [8 0 R] >>"
    objects[8] = b"<< /Type /StructElem /S /P /P 7 0 R /K [7 0 R 8 0 R] >>"
    case("structure-cycle", "structure tree: an element its own kid and its parent's", build(objects))

    objects = page_objects(TEXT)
    depth = 40
    objects[1] = b"<< /Type /Catalog /Pages 2 0 R /MarkInfo << /Marked true >> /StructTreeRoot 6 0 R >>"
    objects[6] = b"<< /Type /StructTreeRoot /K 10 0 R >>"
    for level in range(depth):
        kid = b"%d 0 R" % (11 + level) if level < depth - 1 else b""
        objects[10 + level] = b"<< /Type /StructElem /S /Div /P %d 0 R /K [%s %s] >>" % (
            9 + level if level else 6, kid, kid)
    case("structure-fanout", "structure tree as a DAG: 2^40 paths through 40 elements", build(objects))

    objects = page_objects(TEXT, page_extra=b"/Annots [7 0 R] ")
    objects[1] = b"<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [6 0 R] >> >>"
    objects[6] = b"<< /FT /Tx /T (a) /Kids [7 0 R 6 0 R] /Parent 6 0 R >>"
    objects[7] = b"<< /Type /Annot /Subtype /Widget /Rect [100 100 200 120] /Parent 6 0 R /P 3 0 R >>"
    case("fields-cycle", "form field its own parent and kid", build(objects))

    objects = page_objects(TEXT)
    depth = 40
    objects[1] = b"<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [10 0 R] >> >>"
    for level in range(depth):
        kid = b"%d 0 R" % (11 + level) if level < depth - 1 else b""
        objects[10 + level] = b"<< /T (f%d) /Kids [%s %s] >>" % (level, kid, kid)
    case("fields-fanout", "form fields as a DAG: 2^40 paths", build(objects))

    objects = page_objects(TEXT)
    objects[1] = b"<< /Type /Catalog /Pages 2 0 R /Names << /Dests 6 0 R /EmbeddedFiles 6 0 R >> >>"
    objects[6] = b"<< /Kids [7 0 R] >>"
    objects[7] = b"<< /Kids [6 0 R 7 0 R] >>"
    case("names-cycle", "name tree whose kids loop", build(objects))

    objects = page_objects(TEXT)
    depth = 40
    objects[1] = b"<< /Type /Catalog /Pages 2 0 R /Names << /Dests 10 0 R /EmbeddedFiles 10 0 R >> >>"
    for level in range(depth):
        if level < depth - 1:
            objects[10 + level] = b"<< /Kids [%d 0 R %d 0 R] >>" % (11 + level, 11 + level)
        else:
            objects[10 + level] = b"<< /Names [(a) [3 0 R /Fit]] >>"
    case("names-fanout", "name tree as a DAG: 2^40 paths", build(objects))

    objects = page_objects(TEXT)
    objects[3] = b"<< /Type /Page /Parent 6 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>"
    objects[6] = b"<< /Type /Pages /Parent 6 0 R /Kids [3 0 R] /Count 1 >>"
    case("parent-cycle", "page whose /Parent chain loops (inherited resources)", build(objects))

    objects = page_objects(TEXT, page_extra=b"/Annots [6 0 R] ")
    objects[6] = b"<< /Type /Annot /Subtype /Text /Rect [10 10 30 30] /Popup 7 0 R /IRT 6 0 R >>"
    objects[7] = b"<< /Type /Annot /Subtype /Popup /Rect [10 10 200 100] /Parent 6 0 R /IRT 7 0 R >>"
    case("annotation-cycle", "annotation replying to itself, its popup its own parent", build(objects))

    objects = page_objects(TEXT, page_extra=b"/Annots [" + b"6 0 R " * 1_000_000 + b"] ")
    objects[6] = (b"<< /Type /Annot /Subtype /Square /Rect [100 100 300 300] /F 4 /AP << /N 7 0 R >> >>")
    objects[7] = stream(b"/Type /XObject /Subtype /Form /BBox [0 0 200 200]", b"0 0 200 200 re f")
    case("annotations-many", "one annotation listed a million times on a page", build(objects))


# --- Nesting, sizes read from the file, structure ------------------------------------------------

def structure() -> None:
    depth = 100_000
    objects = page_objects(TEXT)
    objects[1] = b"<< /Type /Catalog /Pages 2 0 R /Deep " + b"[" * depth + b"]" * depth + b" >>"
    case("nesting-arrays", "catalog entry: 100 000 nested arrays", build(objects))

    objects = page_objects(TEXT)
    objects[3] = objects[3].replace(b"/Resources << ", b"/Resources << /Deep " + b"<< /A " * depth + b"1" + b" >>" * depth + b" ")
    case("nesting-dicts", "page resources: 100 000 nested dictionaries", build(objects))

    objects = page_objects(TEXT)
    objects[1] = b"<< /Type /Catalog /Pages 2 0 R /Deep (" + b"(" * depth + b")" * depth + b") >>"
    case("nesting-string", "literal string with 100 000 nested parentheses", build(objects))

    objects = page_objects(b"q " * 1_000_000 + TEXT + b" Q" * 1_000_000)
    case("nesting-q", "page content: a million nested q", build(objects))

    objects = page_objects(b"/P <</MCID 0>> BDC " * 100_000 + TEXT + b" EMC" * 100_000)
    case("nesting-marked", "page content: 100 000 nested marked contents", build(objects))

    objects = page_objects(TEXT, page_extra=b"/Annots [6 0 R] ")
    objects[6] = b"<< /Type /Annot /Subtype /Link /Rect [0 0 20 20] /A 7 0 R >>"
    objects[7] = b"<< /S /URI /URI (https://example.org) /Next [7 0 R 7 0 R] >>"
    case("action-cycle", "a link action whose /Next chain leads back to itself", build(objects))

    objects = page_objects(TEXT + b"\n% 4294967295 0 obj\n")
    case("object-number-max", "an object header of number 2^32 - 1 in a content stream", build(objects))

    objects = page_objects(TEXT)
    objects[6] = b"<< /Title (" + b"A" * (64 << 20) + b") /Producer <" + b"41" * (32 << 20) + b"> >>"
    case("string-huge", "document info: 64 MiB title, 32 MiB hex producer", build(objects, trailer=b"/Root 1 0 R /Info 6 0 R"))

    case("xref-size-huge", "trailer /Size 2^31 - 1", build(page_objects(TEXT), size=2147483647))

    data = simple()
    at = data.rfind(b"startxref")
    loop = data[:at] + b"xref\n0 1\n0000000000 65535 f \ntrailer\n<< /Size 6 /Root 1 0 R /Prev %d >>\nstartxref\n%d\n%%%%EOF\n" % (at, at)
    case("xref-prev-loop", "cross-reference section whose /Prev is itself", loop)

    objects = page_objects(TEXT)
    built = build_xref_stream(objects, {}, xref_entries=b"/Index [0 2000000000] ")
    case("xref-index-huge", "cross-reference stream /Index [0 2e9] with 7 rows", built)

    objects = page_objects(TEXT)
    del objects[3]
    page = b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>"
    objects[6] = stream(b"/Type /ObjStm /N 2000000000 /First 4 /Filter /FlateDecode", deflate(b"3 0 " + page))
    case("objstm-n-huge", "object stream declaring 2e9 objects", build_xref_stream(objects, {3: (6, 0)}))

    filler = b"1 0 obj << /Type /Catalog >> endobj\n" * 2_000_000
    case("repair-scan", "no cross-reference, 2 million object headers to scan (72 MB)",
         b"%PDF-1.7\n" + filler + simple()[9:].split(b"xref")[0] + b"%%EOF\n")

    objects = page_objects(TEXT)
    objects[5] = (b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /FirstChar 0 /LastChar 2147483647 "
                  b"/Widths [500 500] >>")
    case("widths-huge", "simple font /FirstChar 0 /LastChar 2^31 - 1", build(objects))

    objects = page_objects(b"BT /F1 12 Tf 72 700 Td <0001> Tj ET")
    objects[5] = (b"<< /Type /Font /Subtype /Type0 /BaseFont /Cid /Encoding /Identity-H /DescendantFonts [6 0 R] "
                  b"/ToUnicode 8 0 R >>")
    objects[6] = (b"<< /Type /Font /Subtype /CIDFontType2 /BaseFont /Cid /CIDSystemInfo << /Registry (Adobe) "
                  b"/Ordering (Identity) /Supplement 0 >> /FontDescriptor 7 0 R /W [0 4294967295 500 0 [" +
                  b"500 " * 100_000 + b"]] >>")
    objects[7] = (b"<< /Type /FontDescriptor /FontName /Cid /Flags 32 /FontBBox [0 0 1000 1000] /ItalicAngle 0 "
                  b"/Ascent 800 /Descent -200 /CapHeight 700 /StemV 80 >>")
    objects[8] = stream(b"", b"/CIDInit /ProcSet findresource begin 12 dict begin begincmap "
                             b"1 begincodespacerange <0000> <FFFF> endcodespacerange "
                             b"1 beginbfrange <0000> <FFFF> <0041> endbfrange "
                             b"1 beginbfrange <00000000> <FFFFFFFF> <0041> endbfrange endcmap end end")
    case("cid-ranges-huge", "CID font /W range of 2^32 codes, ToUnicode ranges to 2^32", build(objects))

    garbage_ttf = _truetype_garbage()
    objects = page_objects(TEXT)
    objects[5] = (b"<< /Type /Font /Subtype /TrueType /BaseFont /Garbage /FirstChar 32 /LastChar 126 "
                  b"/Widths [" + b"500 " * 95 + b"] /FontDescriptor 6 0 R >>")
    objects[6] = (b"<< /Type /FontDescriptor /FontName /Garbage /Flags 32 /FontBBox [0 0 1000 1000] /ItalicAngle 0 "
                  b"/Ascent 800 /Descent -200 /CapHeight 700 /StemV 80 /FontFile2 7 0 R >>")
    objects[7] = stream(b"", garbage_ttf)
    case("truetype-lies", "TrueType whose tables claim 65535 glyphs and point past the end", build(objects))

    objects = page_objects(TEXT)
    objects[5] = (b"<< /Type /Font /Subtype /Type1 /BaseFont /Garbage /FirstChar 32 /LastChar 126 "
                  b"/Widths [" + b"500 " * 95 + b"] /FontDescriptor 6 0 R >>")
    objects[6] = (b"<< /Type /FontDescriptor /FontName /Garbage /Flags 32 /FontBBox [0 0 1000 1000] /ItalicAngle 0 "
                  b"/Ascent 800 /Descent -200 /CapHeight 700 /StemV 80 /FontFile3 7 0 R >>")
    objects[7] = stream(b"/Subtype /Type1C", _cff_garbage())
    case("cff-lies", "CFF whose INDEX counts and offsets point past the end", build(objects))

    objects = page_objects(b"/Sh0 sh", resources=b"/Shading << /Sh0 6 0 R >>")
    objects[6] = (b"<< /ShadingType 2 /ColorSpace /DeviceGray /Coords [0 0 612 0] /Function 7 0 R >>")
    objects[7] = stream(b"/FunctionType 0 /Domain [0 1] /Range [0 1] /Size [2000000000] /BitsPerSample 32",
                        b"\x00" * 64)
    case("function-samples-huge", "sampled function of 2e9 samples, 64 bytes of data", build(objects))

    mesh = _concat_deflate(b"", b"\x00" + struct.pack(">HHB", 0, 0, 0) + b"\x00" + struct.pack(">HHB", 65535, 0, 128)
                           + b"\x00" + struct.pack(">HHB", 0, 65535, 255), 256 << 20)
    objects = page_objects(b"/Sh0 sh", resources=b"/Shading << /Sh0 6 0 R >>")
    objects[6] = stream(b"/ShadingType 4 /ColorSpace /DeviceGray /BitsPerCoordinate 16 /BitsPerComponent 8 "
                        b"/BitsPerFlag 8 /Decode [0 612 0 792 0 1] /Filter /FlateDecode", mesh)
    case("mesh-huge", "free-form mesh shading: 16 million triangles", build(objects))

    objects = page_objects(TEXT)
    objects[3] = objects[3].replace(b"[0 0 612 792]", b"[0 0 " + b"9" * 60 + b" " + b"9" * 60 + b"] /UserUnit " + b"9" * 60 + b" /Rotate 2147483647")
    case("page-box-huge", "media box and user unit of 1e60, /Rotate 2^31 - 1", build(objects))

    objects = page_objects(TEXT)
    objects[3] = objects[3].replace(b"[0 0 612 792]", b"[0 0 0 0]")
    case("page-box-empty", "media box of zero area", build(objects))

    objects = page_objects(b"BT /F1 24 Tf 72 700 Td (" + b"A" * (64 << 20) + b") Tj ET")
    case("text-huge", "one Tj of 64 MiB", build(objects))

    objects = page_objects(TEXT, page_extra=b"/Annots [6 0 R] ")
    objects[1] = b"<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [6 0 R] /SigFlags 3 >> >>"
    objects[6] = (b"<< /Type /Annot /Subtype /Widget /FT /Sig /T (Signature1) /Rect [0 0 0 0] /P 3 0 R /V 7 0 R >>")
    objects[7] = (b"<< /Type /Sig /Filter /Adobe.PPKLite /SubFilter /ETSI.CAdES.detached "
                  b"/ByteRange [0 9223372036854775807 -5 4611686018427387904] /Contents <" + b"00" * (32 << 20) + b"> >>")
    case("signature-lies", "signature: /ByteRange beyond the file, 32 MiB /Contents", build(objects))

    objects = page_objects(TEXT)
    objects[1] = b"<< /Type /Catalog /Pages 2 0 R /OpenAction << /S /JavaScript /JS (app.alert(1)) >> " \
                 b"/AA << /WC << /S /Launch /F (calc.exe) >> >> >>"
    objects[3] = objects[3].replace(b"/Contents", b"/Annots [6 0 R] /Contents")
    objects[6] = b"<< /Type /Annot /Subtype /Link /Rect [0 0 100 100] /A << /S /URI /URI (http://169.254.169.254/) >> >>"
    case("active-content", "JavaScript, Launch and URI actions", build(objects))


def _truetype_garbage() -> bytes:
    tags = [b"head", b"hhea", b"maxp", b"loca", b"glyf", b"hmtx", b"cmap"]
    directory = struct.pack(">IHHHH", 0x00010000, len(tags), 0, 0, 0)
    body = bytearray()
    offset = 12 + 16 * len(tags)
    tables = {
        b"head": struct.pack(">IIIIHHQQhhhhHHhhh", 0x00010000, 0, 0, 0x5F0F3CF5, 0, 1000, 0, 0, 0, 0, 1000, 1000, 0, 8, 2, 1, 0),
        b"hhea": struct.pack(">IhhhHhhhhhhhhhhhH", 0x00010000, 800, -200, 0, 65535, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 65535),
        b"maxp": struct.pack(">IH", 0x00005000, 65535),
        b"loca": struct.pack(">II", 0, 0xFFFFFFF0),
        b"glyf": struct.pack(">hhhhh", 1000, 0, 0, 100, 100),
        b"hmtx": b"",
        b"cmap": struct.pack(">HHHHI", 0, 1, 3, 1, 0xFFFFFF00),
    }
    entries = bytearray()
    for tag in tags:
        table = tables[tag]
        entries += tag + struct.pack(">III", 0, offset + len(body), len(table) + (1 << 30) * (tag == b"glyf"))
        body += table + b"\x00" * (-len(table) % 4)
    return bytes(directory + entries + body)


def _cff_garbage() -> bytes:
    header = bytes([1, 0, 4, 4])
    name_index = struct.pack(">HB", 1, 1) + bytes([1, 5]) + b"Font"
    top = bytes([29]) + struct.pack(">i", 0x7FFFFFF0) + bytes([17])  # CharStrings offset far away
    top_index = struct.pack(">HB", 1, 1) + bytes([1, 1 + len(top)]) + top
    strings = struct.pack(">HB", 65535, 4) + struct.pack(">I", 1) * 3
    return header + name_index + top_index + strings


# --- XFDF ----------------------------------------------------------------------------------------

XFDF_HEAD = (b'<?xml version="1.0" encoding="UTF-8" ?><xfdf xmlns="http://ns.adobe.com/xfdf/" xml:space="preserve">'
             b'<fields />\n<add>')
XFDF_TAIL = b"</add>\n<modify />\n</xfdf>"


def stamp(image: bytes, page: bytes = b"0", rect: bytes = b"100,100,200,150") -> bytes:
    url = b"data:image/png;base64," + base64.b64encode(image)
    return (b'<stamp page="' + page + b'" rect="' + rect + b'" flags="print,readonly,locked" name="a1b2c3" '
            b'subject="Draft" icon="Draft"><imagedata>' + url + b"</imagedata></stamp>")


def xfdf() -> None:
    small = png(2, 2, deflate(b"\x00\x00\x00" * 2))
    case("benign", "one signature stamp", XFDF_HEAD + stamp(small) + XFDF_TAIL, "xfdf")
    laughs = (b'<?xml version="1.0"?><!DOCTYPE xfdf [<!ENTITY a "aaaaaaaaaa">'
              + b"".join(b'<!ENTITY %c "&%c;&%c;&%c;&%c;&%c;&%c;&%c;&%c;&%c;&%c;">' % ((98 + i,) + (97 + i,) * 10)
                         for i in range(9))
              + b']><xfdf xmlns="http://ns.adobe.com/xfdf/"><add><freetext page="0" rect="0,0,10,10">'
                b"<contents>&j;</contents></freetext></add></xfdf>")
    case("billion-laughs", "entities expanding to 10^10 bytes", laughs, "xfdf")
    xxe = (b'<?xml version="1.0"?><!DOCTYPE xfdf [<!ENTITY x SYSTEM "file:///etc/passwd">]>'
           b'<xfdf xmlns="http://ns.adobe.com/xfdf/"><add><freetext page="0" rect="0,0,10,10">'
           b"<contents>&x;</contents></freetext></add></xfdf>")
    case("external-entity", "external entity reading a local file", xxe, "xfdf")
    case("comment", "a comment swallowing what follows",
         XFDF_HEAD + stamp(small) + b"<!-- " + XFDF_TAIL, "xfdf")
    case("cdata", "a CDATA section", XFDF_HEAD + b"<![CDATA[" + stamp(small) + b"]]>" + XFDF_TAIL, "xfdf")
    case("processing", "a processing instruction", XFDF_HEAD + b"<?evil x?>" + stamp(small) + XFDF_TAIL, "xfdf")
    case("nesting", "100 000 nested elements",
         XFDF_HEAD + b"<a>" * 100_000 + b"</a>" * 100_000 + XFDF_TAIL, "xfdf")
    case("stamps-many", "300 000 stamps", XFDF_HEAD + stamp(small) * 300_000 + XFDF_TAIL, "xfdf")
    case("png-dimensions", "PNG declared 100000 x 100000, 1 KiB of data",
         XFDF_HEAD + stamp(png(100000, 100000, deflate(b"\x00" * 1024))) + XFDF_TAIL, "xfdf")
    case("png-bomb", "PNG of 16384 x 16384 RGB, 768 MiB decoded",
         XFDF_HEAD + stamp(png(16384, 16384, deflate_repeated(b"\x00" + b"\xff" * (16384 * 3), 16384 * (1 + 16384 * 3)),
                               color_type=2)) + XFDF_TAIL, "xfdf")
    case("attribute-huge", "a 64 MiB attribute",
         XFDF_HEAD + b'<stamp page="0" rect="0,0,1,1" name="' + b"a" * (64 << 20) + b'"><imagedata>'
         + b"data:image/png;base64," + base64.b64encode(small) + b"</imagedata></stamp>" + XFDF_TAIL, "xfdf")
    case("rect-nonfinite", "rectangles of NaN, infinity and 1e308",
         XFDF_HEAD + stamp(small, rect=b"nan,inf,-inf,1e308") + stamp(small, rect=b"1e308,1e308,-1e308,-1e308")
         + XFDF_TAIL, "xfdf")
    case("page-huge", "stamps on page 2^64 and -1",
         XFDF_HEAD + stamp(small, page=b"18446744073709551616") + stamp(small, page=b"-1") + XFDF_TAIL, "xfdf")
    case("text-huge", "a free text of 16 MiB",
         XFDF_HEAD + b'<freetext page="0" rect="100,500,220,525" flags="print,readonly,locked" '
         b'defaultappearance="0 0 0 rg /Helvetica 16 Tf"><contents>' + b"A " * (8 << 20)
         + b"</contents></freetext>" + XFDF_TAIL, "xfdf")


def main() -> None:
    out = Path(sys.argv[1])
    bombs()
    graphs()
    structure()
    xfdf()
    (out / "pdf").mkdir(parents=True, exist_ok=True)
    (out / "xfdf").mkdir(parents=True, exist_ok=True)
    for name, data in PDFS.items():
        (out / "pdf" / f"{name}.pdf").write_bytes(data)
    for name, data in XFDFS.items():
        (out / "xfdf" / f"{name}.xfdf").write_bytes(data)
    (out / "benign.pdf").write_bytes(simple())
    (out / "cases.json").write_text(json.dumps(CASES, indent=1))
    print(f"{len(PDFS)} PDF, {len(XFDFS)} XFDF into {out}")


if __name__ == "__main__":
    main()
