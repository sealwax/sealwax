"""One page rendered to a colour PNG, from Python."""

import struct
import zlib

from conftest import build, stream

import sealwax


def size_and_pixel(png: bytes, x: int, y: int) -> tuple[int, int, tuple[int, ...]]:
    """The width, height and one pixel of an 8-bit RGB PNG (non-interlaced)."""
    assert png[:8] == b"\x89PNG\r\n\x1a\n"
    width, height, depth, color = struct.unpack(">IIBB", png[16:26])
    assert (depth, color) == (8, 2), "8-bit RGB"
    data, at = b"", 8
    while at < len(png):
        (length,) = struct.unpack(">I", png[at:at + 4])
        kind = png[at + 4:at + 8]
        if kind == b"IDAT":
            data += png[at + 8:at + 8 + length]
        at += 12 + length
    raw = zlib.decompress(data)
    stride = 1 + 3 * width
    rows: list[bytearray] = []
    for row in range(height):
        kind, line = raw[row * stride], bytearray(raw[row * stride + 1:(row + 1) * stride])
        previous = rows[-1] if rows else bytearray(3 * width)
        for i in range(len(line)):
            left = line[i - 3] if i >= 3 else 0
            up = previous[i]
            corner = previous[i - 3] if i >= 3 else 0
            if kind == 1:
                line[i] = (line[i] + left) & 255
            elif kind == 2:
                line[i] = (line[i] + up) & 255
            elif kind == 3:
                line[i] = (line[i] + (left + up) // 2) & 255
            elif kind == 4:
                p = left + up - corner
                pa, pb, pc = abs(p - left), abs(p - up), abs(p - corner)
                predictor = left if pa <= pb and pa <= pc else (up if pb <= pc else corner)
                line[i] = (line[i] + predictor) & 255
        rows.append(line)
    return width, height, tuple(rows[y][3 * x:3 * x + 3])


def page() -> bytes:
    return build([
        "<< /Type /Catalog /Pages 2 0 R >>",
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] /CropBox [50 70 550 770] /Rotate 90 "
        "/Contents 4 0 R >>",
        stream("", b"1 0 0 rg 50 70 250 700 re f"),
    ])


def test_the_image_covers_the_rotated_crop_box_at_the_scale_asked():
    document = sealwax.Document.from_bytes(page())
    width, height, top = size_and_pixel(document.render_png(0, 150 / 72), 729, 10)
    assert (width, height) == (1459, 1042), "rounded up, as PyMuPDF sizes its pixmaps"
    assert top == (255, 0, 0)
    assert size_and_pixel(document.render_png(0, 150 / 72), 729, 1030)[2] == (255, 255, 255)


def test_past_the_pixel_bound_the_limit_is_named():
    document = sealwax.Document.from_bytes(page(), limits=sealwax.Limits(max_render_pixels=100_000))
    try:
        document.render_png(0, 1.0)
    except sealwax.PdfLimitError as error:
        assert error.limit == "max_render_pixels"
    else:
        raise AssertionError("not refused")
    assert sealwax.Limits(max_render_pixels=7).max_render_pixels == 7
    for scale in (0.0, -1.0, float("nan")):
        try:
            document.render_png(0, scale)
        except ValueError:
            pass
        else:
            raise AssertionError(f"scale {scale} accepted")


def test_pixels_for_a_canvas_and_quarter_turns():
    document = sealwax.Document.from_bytes(build([
        "<< /Type /Catalog /Pages 2 0 R >>",
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Contents 4 0 R >>",
        stream("", b"1 0 0 rg 0 0 100 100 re f"),
    ]))
    width, height, pixels = document.render_rgba(0, 1.0)
    assert (width, height, len(pixels)) == (200, 100, 200 * 100 * 4)
    assert pixels[:4] == bytes([255, 0, 0, 255])
    width, height, _ = document.render_rgba(0, 1.0, quarter_turns=1)
    assert (width, height) == (100, 200)
    assert document.render_png(0, 1.0, quarter_turns=2)[1:4] == b"PNG"


def test_annotations_made_are_drawn_and_kept_on_save():
    document = sealwax.Document.from_bytes(build([
        "<< /Type /Catalog /Pages 2 0 R >>",
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] >>",
    ]))
    document.add_annotation(0, "square", rect=(10, 10, 90, 90), color=(1, 0, 0), fill=(0, 0, 1), width=4)
    document.add_annotation(0, "highlight", quads=[(110, 60, 190, 60, 110, 40, 190, 40)], author="Zoé")
    document.add_annotation(0, "line", points=[(110, 20), (190, 20)], width=2)
    for bad in [{"kind": "line", "points": [(0, 0)]}, {"kind": "ink"}, {"kind": "arrow"}]:
        kind = bad.pop("kind")
        try:
            document.add_annotation(0, kind, **bad)  # type: ignore[arg-type]
        except ValueError:
            pass
        else:
            raise AssertionError(f"{kind} accepted")
    width, height, pixels = sealwax.Document.from_bytes(document.save()).render_rgba(0, 1.0)
    at = lambda x, y: tuple(pixels[4 * (y * width + x): 4 * (y * width + x) + 3])  # noqa: E731
    assert at(50, 50) == (0, 0, 255), "the square's inside"
    assert at(150, 50) == (255, 255, 0), "the highlight"
    assert at(150, 80)[0] < 80, "the line"
