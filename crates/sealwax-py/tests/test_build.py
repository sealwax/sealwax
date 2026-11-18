"""Documents built from nothing, as test fixtures are: pages, text, lines, rectangles, links,
fields, an outline, information and XMP, read back once saved."""

import pytest

import sealwax

XMP = (
    b'<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">'
    b'<rdf:Description rdf:about="" xmlns:pdfaid="http://www.aiim.org/pdfa/ns/id/" '
    b'pdfaid:part="2" pdfaid:conformance="B"/></rdf:RDF></x:xmpmeta>'
)


def lines_of_text(document, page_index=0):
    """Each line of text: its text, font, size, and its first glyph's origin, by baseline."""
    lines = {}
    for glyph in document.glyphs(page_index):
        lines.setdefault(round(glyph["origin"][1], 3), []).append(glyph)
    return [
        ("".join(g["text"] or "" for g in glyphs), glyphs[0]["font"], round(glyphs[0]["size"], 3),
         round(glyphs[0]["origin"][0], 3), y)
        for y, glyphs in sorted(lines.items(), reverse=True)
    ]


def test_a_document_built_reads_back_as_built():
    document = sealwax.Document.new()
    page = document.add_page(600, 800)
    assert document.add_page() == 1
    assert document.insert_text(page, (80, 100), "Example title", font="hebo", size=14) == 0
    assert document.insert_text(page, (80, 150), "Éloïse ✓\nsecond line") == 1
    document.draw_line(page, (50, 200), (250, 200), width=2)
    document.draw_rect(page, (50, 300, 250, 400))
    document.draw_rect(page, (300, 300, 400, 350), color=None, fill=(0.9, 0.9, 0.9))
    document.add_link(page, (80, 500, 200, 520), "https://example.org/")
    document.add_field("name", "text", page, (80, 600, 300, 620))
    document.set_outline([(1, "Chapter", 1), (2, "Section", 2), (1, "No page", None)])
    document.set_info({"Title": "Fixture", "Author": "Tests", "CreationDate": "D:20240131120000Z"})
    document.set_xmp(XMP)
    built = sealwax.Document.from_bytes(document.save())

    assert built.page_count == 2
    assert built.geometry()[1].media_box == (0.0, 0.0, 595.0, 842.0)
    # As PyMuPDF draws the same: baselines from the top, each line the font's height below.
    assert lines_of_text(built) == [
        ("Example title", "Helvetica-Bold", 14.0, 80.0, 700.0),
        ("Éloïse ?", "Helvetica", 11.0, 80.0, 650.0),
        ("second line", "Helvetica", 11.0, 80.0, 634.886),
    ]
    drawings = built.drawings(0)
    assert drawings["lines"] == [(50.0, 600.0, 250.0, 600.0)]
    assert (50.0, 400.0, 250.0, 500.0) in drawings["rectangles"]
    assert [(f["name"], f["kind"]) for f in built.fields()] == [("name", "text")]
    assert built.outline() == [
        {"level": 1, "title": "Chapter", "page_number": 1},
        {"level": 2, "title": "Section", "page_number": 2},
        {"level": 1, "title": "No page", "page_number": None},
    ]
    metadata = built.metadata()
    assert metadata["pdfa"] == "2B"
    assert metadata["info"] == {"Title": "Fixture", "Author": "Tests", "CreationDate": "D:20240131120000Z"}


@pytest.mark.parametrize("turns", [0, 1, 2, 3])
def test_positions_are_on_the_page_as_it_shows_and_text_reads_upright(turns):
    document = sealwax.Document.new()
    page = document.add_page(600, 800)
    # As PyMuPDF's set_cropbox: from the top-left corner of the media box, y down.
    document.set_crop_box(page, (50, 70, 550, 770))
    document.rotate_pages([(page, turns)])
    document.insert_text(page, (80, 100), "Upright", size=20)
    built = sealwax.Document.from_bytes(document.save())
    assert built.geometry()[0].crop_box == (50.0, 30.0, 550.0, 730.0)
    glyph = built.glyphs(0)[0]
    # The point shown at (80, 100), the baseline going right as the page shows.
    expected = {
        0: ((130.0, 630.0), (1.0, 0.0)),
        1: ((150.0, 110.0), (0.0, 1.0)),
        2: ((470.0, 130.0), (-1.0, 0.0)),
        3: ((450.0, 650.0), (0.0, -1.0)),
    }[turns]
    assert (round(glyph["origin"][0], 3), round(glyph["origin"][1], 3)) == expected[0]
    assert (round(glyph["direction"][0], 6) + 0.0, round(glyph["direction"][1], 6) + 0.0) == expected[1]


def test_what_cannot_be_built_is_refused():
    document = sealwax.Document.new()
    with pytest.raises(sealwax.PdfError):
        document.save()
    with pytest.raises(ValueError):
        document.add_page(0, 100)
    page = document.add_page()
    with pytest.raises(ValueError):
        document.insert_text(page, (10, 10), "x", font="Symbol")
    with pytest.raises(ValueError):
        document.insert_text(page, (10, 10), "x", color=(2, 0, 0))
    with pytest.raises(ValueError):
        document.draw_rect(page, (0, 0, 10, 10), color=None)
    with pytest.raises(ValueError):
        document.set_crop_box(page, (700, 900, 800, 1000))
    with pytest.raises(ValueError):
        document.set_outline([(2, "Too deep", 1)])
    with pytest.raises(ValueError):
        document.set_outline([(1, "Nowhere", 2)])
    with pytest.raises(ValueError):
        document.add_link(page, (10, 10, 10, 20), "https://example.org/")


def test_fields_of_every_kind_and_their_retouches_read_back():
    document = sealwax.Document.new()
    page = document.add_page(600, 800)
    document.add_field("country", "combobox", page, (80, 100, 300, 120),
                       choices=[("fr", "France"), "Other"], label="Country")
    document.add_field("sign", "signature", page, (80, 200, 300, 260))
    document.add_field("code", "text", page, (80, 300, 200, 320), max_length=5, label="Code")
    document.add_field("agree", "checkbox", page, (80, 400, 95, 415))
    document.rename_button_state("agree", b"99/99")
    document.set_field_label("code", "Postal code")
    assert document.remove_field_appearances("code") == 1
    fields = {f["name"]: f for f in sealwax.Document.from_bytes(document.save()).fields()}
    assert fields["country"]["kind"] == "combo_box"
    assert fields["country"]["choices"] == [("fr", "France"), "Other"]
    assert fields["country"]["label"] == "Country" and fields["country"]["value"] is None
    assert fields["sign"]["kind"] == "signature"
    assert fields["code"]["max_length"] == 5 and fields["code"]["label"] == "Postal code"
    assert fields["agree"]["button_states"] == [b"99/99", b"Off"]
    with pytest.raises(ValueError):
        document.add_field("list", "combobox", page, (80, 500, 300, 520))
    with pytest.raises(ValueError):
        document.add_field("box", "checkbox", page, (80, 500, 95, 515), max_length=3)
    with pytest.raises(ValueError):
        document.rename_button_state("code", b"On")
    with pytest.raises(ValueError):
        document.set_field_label("nobody", "x")


@pytest.mark.parametrize("encryption, description", [
    ("rc4-128", "Standard V2 R3 128-bit RC4"),
    ("aes-128", "Standard V4 R4 128-bit AES"),
    ("aes-256", "Standard V5 R6 256-bit AES"),
])
def test_a_document_saved_encrypted_opens_with_its_password(encryption, description):
    document = sealwax.Document.new()
    page = document.add_page()
    document.insert_text(page, (80, 100), "Secret")
    document.set_info({"Title": "Chiffré"})
    # Without a user password, it opens as it is.
    opened = sealwax.Document.from_bytes(
        document.save(encryption=encryption, owner_password="owner",
                      permissions=4 | 256))
    assert opened.metadata()["encryption"] == description
    assert "".join(g["text"] or "" for g in opened.glyphs(0)) == "Secret"
    # With one, it is asked for.
    locked = document.save(encryption=encryption, owner_password="owner", user_password="user")
    with pytest.raises(sealwax.PdfPasswordError):
        sealwax.Document.from_bytes(locked)
    with pytest.raises(sealwax.PdfPasswordError):
        sealwax.Document.from_bytes(locked, password="wrong")
    unlocked = sealwax.Document.from_bytes(locked, password="user")
    assert unlocked.metadata()["info"] == {"Title": "Chiffré"}
    # Two saves draw their keys anew.
    assert document.save(encryption=encryption, owner_password="owner") != \
        document.save(encryption=encryption, owner_password="owner")


def test_encryption_options_it_cannot_take_are_refused():
    document = sealwax.Document.new()
    document.add_page()
    with pytest.raises(ValueError):
        document.save(encryption="aes-512", owner_password="owner")
    with pytest.raises(ValueError):
        document.save(encryption="aes-256")
    with pytest.raises(ValueError):
        document.save(encryption="aes-256", owner_password="owner", permissions=1)
    with pytest.raises(ValueError):
        document.save(user_password="user")
