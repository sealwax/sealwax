"""Form fields: read as their widgets place them, filled, read back, flattened alone."""

import hashlib

from conftest import build, stream

import sealwax

APPEARANCE = stream("/Type /XObject /Subtype /Form /BBox [0 0 10 10]", b"0 0 10 10 re f")
EMPTY = stream("/Type /XObject /Subtype /Form /BBox [0 0 10 10]", b"")


def form() -> bytes:
    """Two pages. `name` (text, required, MaxLen 12) has a widget on each; `agree` a check box;
    `lang` radio buttons, one state `/Fran#E7ais`; `country` a list box; `bare` a check box
    without appearance; `sign` a signature field. Page 1 also has a link and a comment."""
    return build([
        "<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [7 0 R 10 0 R 11 0 R 14 0 R 15 0 R 16 0 R] "
        "/DA (/Helv 0 Tf 0 g) >> >>",
        "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>",
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] /CropBox [50 70 550 770] /Contents 5 0 R "
        "/Annots [8 0 R 10 0 R 12 0 R 13 0 R 14 0 R 15 0 R 16 0 R 17 0 R 18 0 R] >>",
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] /Contents 6 0 R /Annots [9 0 R] >>",
        stream("", b"BT /F1 12 Tf 60 700 Td (Original) Tj ET"),
        stream("", b""),
        "<< /FT /Tx /T (name) /TU (Your name) /Ff 2 /MaxLen 12 /Kids [8 0 R 9 0 R] >>",
        "<< /Type /Annot /Subtype /Widget /Parent 7 0 R /Rect [100 400 200 420] /F 4 >>",
        "<< /Type /Annot /Subtype /Widget /Parent 7 0 R /Rect [100 400 200 420] /F 4 >>",
        "<< /Type /Annot /Subtype /Widget /FT /Btn /T (agree) /Rect [100 300 112 312] /F 4 /AS /Off "
        "/AP << /N << /Yes 19 0 R /Off 20 0 R >> >> >>",
        "<< /FT /Btn /Ff 32768 /T (lang) /Kids [12 0 R 13 0 R] >>",
        "<< /Type /Annot /Subtype /Widget /Parent 11 0 R /Rect [100 250 112 262] /F 4 /AS /Off "
        "/AP << /N << /en 19 0 R /Off 20 0 R >> >> /MK << /CA (l) >> >>",
        "<< /Type /Annot /Subtype /Widget /Parent 11 0 R /Rect [120 250 132 262] /F 4 /AS /Off "
        "/AP << /N << /Fran#E7ais 19 0 R /Off 20 0 R >> >> >>",
        "<< /Type /Annot /Subtype /Widget /FT /Ch /T (country) /Opt [[(CA) (Canada)] [(FR) (France)]] "
        "/Rect [100 200 250 220] /F 4 >>",
        "<< /Type /Annot /Subtype /Widget /FT /Btn /T (bare) /Rect [300 300 312 312] /F 4 >>",
        "<< /Type /Annot /Subtype /Widget /FT /Sig /T (sign) /Rect [300 100 400 150] /F 4 >>",
        "<< /Type /Annot /Subtype /Link /Rect [60 600 200 620] /A << /S /URI /URI (https://example.org) >> >>",
        "<< /Type /Annot /Subtype /Text /Rect [400 600 420 620] /Contents (A comment) /F 4 >>",
        APPEARANCE,
        EMPTY,
    ])


def test_every_widget_is_an_occurrence_with_what_its_field_says():
    document = sealwax.Document.from_bytes(form())
    fields = document.fields()
    assert [(f["name"], f["page_number"]) for f in fields] == [
        ("name", 1), ("agree", 1), ("lang", 1), ("lang", 1), ("country", 1), ("bare", 1), ("sign", 1), ("name", 2),
    ]
    name = fields[0]
    assert (name["kind"], name["label"], name["required"], name["read_only"], name["max_length"]) == (
        "text", "Your name", True, False, 12,
    )
    assert name["rect"] == (100, 400, 200, 420) and name["crop_box"] == (50, 70, 550, 770)
    assert fields[1]["kind"] == "checkbox" and fields[1]["button_states"] == [b"Off", b"Yes"]
    assert fields[1]["state"] == b"Off"
    assert fields[2]["kind"] == "radio" and fields[2]["button_caption"] == "l"
    assert fields[3]["button_states"] == [b"Fran\xe7ais", b"Off"]
    assert fields[4]["kind"] == "list_box" and fields[4]["choices"] == [("CA", "Canada"), ("FR", "France")]
    assert fields[5]["button_states"] == []
    assert fields[6]["kind"] == "signature"
    assert len(document.fields(1)) == 1


def test_values_written_read_back_with_button_names_kept_to_the_byte():
    document = sealwax.Document.from_bytes(form())
    filled = document.fill_fields({
        "name": "Élodie",
        "agree": "Yes",
        "lang": b"Fran\xe7ais",
        "country": "FR",
        "bare": "Yes",
        "sign": "never written",
    })
    assert filled == {"occurrences": 7, "characters_replaced": 0, "unmatched": ["bare"]}
    plain = document.save(xref="table")
    assert b"/Fran#E7ais" in plain, "the state is written with its own bytes"
    reopened = sealwax.Document.from_bytes(document.save())
    assert reopened.field_values() == {
        "name": ["Élodie", "Élodie"],
        "agree": ["Yes"],
        "lang": ["Off", "Français"],
        "country": ["FR"],
        "bare": ["Off"],
        "sign": [""],
    }
    assert reopened.fields()[3]["state"] == b"Fran\xe7ais"
    # An empty value clears the text on every occurrence.
    assert reopened.fill_fields({"name": ""})["occurrences"] == 2
    assert sealwax.Document.from_bytes(reopened.save()).field_values()["name"] == ["", ""]


def test_a_state_given_as_text_the_document_spells_otherwise_is_reported():
    document = sealwax.Document.from_bytes(form())
    # The document spells the state in Latin-1 (b"Fran\xe7ais"); its UTF-8 text names none.
    assert document.fill_fields({"lang": "Français"})["unmatched"] == ["lang"]
    assert document.fill_fields({"lang": b"Fran\xe7ais"})["unmatched"] == []


def test_characters_the_font_lacks_are_counted_never_lost():
    document = sealwax.Document.from_bytes(form())
    assert document.fill_fields({"name": "Ωmega"}) == {"occurrences": 2, "characters_replaced": 2, "unmatched": []}
    assert sealwax.Document.from_bytes(document.save()).field_values()["name"] == ["Ωmega", "Ωmega"]


def test_flattening_the_fields_alone_keeps_links_and_comments():
    document = sealwax.Document.from_bytes(form())
    document.fill_fields({"name": "Alice"})
    report = document.flatten(fields_only=True)
    assert report["painted"] >= 2
    flattened = sealwax.Document.from_bytes(document.save(xref="table"))
    assert flattened.fields() == []
    data = flattened.save(xref="table")
    assert b"/Subtype /Link" in data and b"/Subtype /Text" in data and b"/AcroForm" not in data


def _rc4(key: bytes, data: bytes) -> bytes:
    state = list(range(256))
    j = 0
    for i in range(256):
        j = (j + state[i] + key[i % len(key)]) % 256
        state[i], state[j] = state[j], state[i]
    out = bytearray()
    i = j = 0
    for byte in data:
        i = (i + 1) % 256
        j = (j + state[i]) % 256
        state[i], state[j] = state[j], state[i]
        out.append(byte ^ state[(state[i] + state[j]) % 256])
    return bytes(out)


def encrypted_form() -> bytes:
    """A text field `name` in a document encrypted with RC4 40-bit (standard security handler,
    revision 2) and an empty user password: it opens without one."""
    padding = bytes.fromhex("28BF4E5E4E758A4164004E56FFFA01082E2E00B6D0683E802F0CA9FE6453697A")
    document_id = bytes(range(16))
    owner = _rc4(hashlib.md5(padding).digest()[:5], padding)
    permissions = -4
    key = hashlib.md5(padding + owner + permissions.to_bytes(4, "little", signed=True) + document_id).digest()[:5]
    user = _rc4(key, padding)

    def encrypted(number: int, data: bytes) -> bytes:
        object_key = hashlib.md5(key + number.to_bytes(3, "little") + b"\x00\x00").digest()[:10]
        return _rc4(object_key, data)

    def text(number: int, value: bytes) -> str:
        return "<" + encrypted(number, value).hex() + ">"

    content = encrypted(4, b"BT /F1 12 Tf 60 700 Td (Hi) Tj ET")
    trailer = (
        f"/Encrypt 7 0 R /ID [<{document_id.hex()}> <{document_id.hex()}>]"
    )
    return build([
        "<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [5 0 R] >> >>",
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] /Contents 4 0 R /Annots [5 0 R] >>",
        stream("", content),
        f"<< /Type /Annot /Subtype /Widget /FT /Tx /T {text(5, b'name')} /Rect [100 400 200 420] /F 4 >>",
        "<< >>",
        f"<< /Filter /Standard /V 1 /R 2 /Length 40 /P {permissions} /O <{owner.hex()}> /U <{user.hex()}> >>",
    ], trailer=trailer)


def test_an_encrypted_document_without_user_password_is_filled_and_saved():
    document = sealwax.Document.from_bytes(encrypted_form())
    assert [field["name"] for field in document.fields()] == ["name"]
    assert document.fill_fields({"name": "Alice"})["occurrences"] == 1
    saved = document.save()
    assert sealwax.Document.from_bytes(saved).field_values() == {"name": ["Alice"]}


def test_a_value_of_another_type_is_refused():
    document = sealwax.Document.from_bytes(form())
    try:
        document.fill_fields({"name": 12})  # type: ignore[dict-item]
    except ValueError as error:
        assert "str or bytes" in str(error)
    else:
        raise AssertionError("not refused")
