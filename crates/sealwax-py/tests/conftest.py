import base64
import struct
import zlib


def build(objects: list[str | bytes], trailer: str = "") -> bytes:
    """A PDF of `objects` (numbered from 1, the first the catalog) with an exact xref table."""
    out = bytearray(b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n")
    offsets = []
    for number, body in enumerate(objects, start=1):
        offsets.append(len(out))
        body = body.encode() if isinstance(body, str) else body
        out += f"{number} 0 obj\n".encode() + body + b"\nendobj\n"
    xref_at = len(out)
    out += f"xref\n0 {len(objects) + 1}\n0000000000 65535 f \n".encode()
    for offset in offsets:
        out += f"{offset:010} 00000 n \n".encode()
    out += f"trailer\n<< /Size {len(objects) + 1} /Root 1 0 R {trailer} >>\nstartxref\n{xref_at}\n%%EOF\n".encode()
    return bytes(out)


def stream(entries: str, data: bytes) -> bytes:
    return f"<< {entries} /Length {len(data)} >>\nstream\n".encode() + data + b"\nendstream"


def text_page(content: bytes = b"BT /F1 24 Tf 72 700 Td (Hello) Tj ET", annots: str = "") -> bytes:
    """One page of text, Helvetica, with `annots` (an /Annots value) if given."""
    return build([
        "<< /Type /Catalog /Pages 2 0 R >>",
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R "
        f"/Resources << /Font << /F1 5 0 R >> >> {'/Annots ' + annots if annots else ''} >>",
        stream("", content),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
        "<< /Type /Annot /Subtype /Square /Rect [100 100 200 200] /F 4 /C [1 0 0] >>",
    ])


def png(width: int = 2, height: int = 2) -> bytes:
    def chunk(kind: bytes, body: bytes) -> bytes:
        return struct.pack(">I", len(body)) + kind + body + struct.pack(">I", zlib.crc32(kind + body))
    rows = b"".join(b"\x00" + b"\x00\x00\x00" * width for _ in range(height))
    header = struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0)
    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", header) + chunk(b"IDAT", zlib.compress(rows)) + chunk(b"IEND", b"")


def xfdf(page: int = 0, rect: str = "100,100,200,150") -> str:
    url = "data:image/png;base64," + base64.b64encode(png()).decode()
    return (
        '<?xml version="1.0" encoding="UTF-8" ?><xfdf xmlns="http://ns.adobe.com/xfdf/" xml:space="preserve">'
        f'<fields />\n<add><stamp page="{page}" rect="{rect}" flags="print,readonly,locked" name="a1" '
        f'subject="Draft" icon="Draft"><imagedata>{url}</imagedata></stamp></add>\n<modify />\n</xfdf>'
    )
