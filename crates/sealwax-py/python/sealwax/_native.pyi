from collections.abc import Callable
from os import PathLike
from typing import Any, Literal, NotRequired, TypedDict, final

__version__: str

class PdfError(Exception): ...
class PdfReadError(PdfError): ...
class PdfPasswordError(PdfReadError): ...
class PdfEncryptedError(PdfReadError): ...

class PdfLimitError(PdfReadError):
    limit: str

class PdfTimeoutError(PdfLimitError): ...
class XfdfError(PdfError): ...
class PdfAError(PdfError): ...
class FieldNameTakenError(PdfError): ...
class SignatureError(PdfError): ...
class TimestampError(SignatureError): ...
class ValidationError(SignatureError): ...

class Flattened(TypedDict):
    painted: int
    drawn: int
    links_kept: int
    dropped: dict[str, int]
    characters_replaced: int

class Applied(TypedDict):
    pictures: int
    texts: int
    inks: int
    characters_replaced: int
    deletions_ignored: int
    annotations_removed: int
    pages: list[int]
    flattened: Flattened | None

class PdfAReport(TypedDict):
    changes: dict[str, int]
    residual: dict[str, int]

class Peaks(TypedDict):
    operators: int
    shown_bytes: int
    page_cost: int
    image_pixels: int
    mesh_bytes: int
    code_range: int
    stream_bytes: int
    dash_period: float | None
    annotations: int

class Field(TypedDict):
    name: str
    label: str | None
    kind: str
    page_number: int
    rect: tuple[float, float, float, float]
    crop_box: tuple[float, float, float, float]
    value: str | list[str] | None
    state: bytes | None
    required: bool
    read_only: bool
    max_length: int | None
    editable: bool
    choices: list[str | tuple[str, str]]
    button_states: list[bytes]
    button_caption: str | None

class Glyph(TypedDict):
    text: str | None
    origin: tuple[float, float]
    end: tuple[float, float]
    bbox: tuple[float, float, float, float]
    ink: tuple[float, float, float, float] | None
    size: float
    direction: tuple[float, float]
    font: str | None
    invisible: bool
    overdrawn: bool
    actual_text: str | None
    annotation: bool

class Drawings(TypedDict):
    lines: list[tuple[float, float, float, float]]
    rectangles: list[tuple[float, float, float, float]]
    quads: list[tuple[tuple[float, float], tuple[float, float], tuple[float, float], tuple[float, float]]]

class OutlineEntry(TypedDict):
    level: int
    title: str
    page_number: int | None

class PageImage(TypedDict):
    bbox: tuple[float, float, float, float]
    transform: tuple[float, float, float, float, float, float]
    width: int
    height: int
    xref: int | None
    color_space: str | None
    bits_per_component: int | None
    filters: list[str]
    stencil: bool
    masked: bool

class ExtractedImage(TypedDict):
    ext: Literal["jpeg", "jpx", "png"]
    image: bytes
    width: int
    height: int

class Attachment(TypedDict):
    name: str
    key: str | None
    description: str | None
    mime_type: str | None
    size: int | None
    created: str | None
    modified: str | None
    relationship: str | None
    page_number: int | None
    rect: tuple[float, float, float, float] | None

class TextWord(TypedDict):
    text: str
    bbox: tuple[float, float, float, float]
    block: int
    line: int
    word: int

class Link(TypedDict):
    rect: tuple[float, float, float, float]
    kind: Literal["page", "uri", "file", "named", "unknown"]
    page_number: int | None
    uri: str | None
    file: str | None
    name: str | None

class Metadata(TypedDict):
    version: str
    encrypted: bool
    encryption: str | None
    linearized: bool
    has_forms: bool
    has_signatures: bool
    pdfa: str | None
    info: dict[str, str]

class DocxReport(TypedDict):
    skipped: dict[str, int]
    not_described: dict[str, int]

class PhotoPages(TypedDict):
    added: int
    unreadable: list[int]

class Written(TypedDict):
    value: str
    previous: str

class Filled(TypedDict):
    occurrences: int
    characters_replaced: int
    refused: list[str]
    kept: list[str]
    written: dict[str, Written]

class Redacted(TypedDict):
    glyphs: int
    operators: int
    forms: int
    strings_unread: int
    images: int
    images_removed: int
    paths: int
    replacement_texts: int

class AppliedRedactions(TypedDict):
    marks: int
    pages: int
    links_removed: int
    redacted: Redacted

class TemplateContent(TypedDict):
    pages: int
    fields: int
    annotations: int

class Cleared(TypedDict):
    fields: int
    annotations_removed: int

class Coverage(TypedDict):
    certificates: int
    ocsp_responses: int
    crls: int

class SignatureCheck(TypedDict):
    field: str
    kind: str
    sub_filter: str
    signer: str | None
    timestamp_time: int | None
    later_revisions: int
    integrity: str
    detail: str | None

@final
class Limits:
    def __init__(
        self,
        *,
        timeout: float | None = None,
        max_operators: int | None = None,
        max_shown_bytes: int | None = None,
        max_page_cost: int | None = None,
        max_image_pixels: int | None = None,
        max_mesh_bytes: int | None = None,
        max_code_range: int | None = None,
        max_stream_bytes: int | None = None,
        min_dash_period: float | None = None,
        max_annotations: int | None = None,
        max_render_pixels: int | None = None,
        max_unpacked_bytes: int | None = None,
    ) -> None: ...
    @staticmethod
    def unbounded(*, timeout: float | None = None) -> Limits: ...
    @property
    def timeout(self) -> float | None: ...
    @property
    def max_operators(self) -> int: ...
    @property
    def max_shown_bytes(self) -> int: ...
    @property
    def max_page_cost(self) -> int: ...
    @property
    def max_image_pixels(self) -> int: ...
    @property
    def max_mesh_bytes(self) -> int: ...
    @property
    def max_code_range(self) -> int: ...
    @property
    def max_stream_bytes(self) -> int: ...
    @property
    def min_dash_period(self) -> float: ...
    @property
    def max_annotations(self) -> int: ...
    @property
    def max_render_pixels(self) -> int: ...
    @property
    def max_unpacked_bytes(self) -> int: ...

@final
class PageGeometry:
    @property
    def page_number(self) -> int: ...
    @property
    def crop_box(self) -> tuple[float, float, float, float]: ...
    @property
    def media_box(self) -> tuple[float, float, float, float]: ...
    @property
    def rotation(self) -> int: ...
    @property
    def user_unit(self) -> float: ...

@final
class Document:
    @staticmethod
    def from_bytes(
        data: bytes,
        *,
        password: str | None = None,
        page_count: tuple[int, int] | None = None,
        limits: Limits | None = None,
        refuse_encrypted: bool = False,
    ) -> Document: ...
    @staticmethod
    def open(
        path: str | PathLike[str],
        *,
        password: str | None = None,
        page_count: tuple[int, int] | None = None,
        limits: Limits | None = None,
        refuse_encrypted: bool = False,
    ) -> Document: ...
    @property
    def page_count(self) -> int: ...
    @property
    def is_modified(self) -> bool: ...
    def geometry(self) -> list[PageGeometry]: ...
    def save(
        self,
        *,
        xref: str = "stream",
        encryption: str | None = None,
        owner_password: str | None = None,
        user_password: str = "",
        permissions: int | None = None,
    ) -> bytes: ...
    def save_incremental(self) -> bytes: ...
    def flatten(self, *, fields_only: bool = False) -> Flattened: ...
    def fields(self, page_index: int | None = None) -> list[Field]: ...
    def fill_fields(
        self, values: dict[str, str | bytes | bool], *, keep_filled: bool = False, strict: bool = False
    ) -> Filled: ...
    def unfill_fields(self, written: dict[str, Written]) -> list[str]: ...
    def clear_template(self, title: str, *, keep_annotations: bool = False) -> Cleared: ...
    def template_content(self) -> TemplateContent: ...
    def add_annotation(
        self,
        page_index: int,
        kind: str,
        *,
        quads: list[tuple[float, float, float, float, float, float, float, float]] | None = None,
        strokes: list[list[tuple[float, float]]] | None = None,
        points: list[tuple[float, float]] | None = None,
        rect: tuple[float, float, float, float] | None = None,
        text: str | None = None,
        size: float = 12.0,
        color: tuple[float, float, float] | None = None,
        fill: tuple[float, float, float] | None = None,
        width: float = 1.0,
        opacity: float = 1.0,
        author: str | None = None,
        contents: str | None = None,
    ) -> None: ...
    def add_field(
        self,
        name: str,
        kind: str,
        page_index: int,
        rect: tuple[float, float, float, float],
        *,
        required: bool = False,
        font_size: float | None = None,
        choices: list[str | tuple[str, str]] | None = None,
        label: str | None = None,
        max_length: int | None = None,
    ) -> None: ...
    def add_widget(self, name: str, page_index: int, rect: tuple[float, float, float, float]) -> None: ...
    def set_field_label(self, name: str, label: str | None) -> None: ...
    def rename_button_state(self, name: str, state: bytes) -> None: ...
    def remove_field_appearances(self, name: str) -> int: ...
    @staticmethod
    def new(*, limits: Limits | None = None) -> Document: ...
    def add_page(self, width: float = 595.0, height: float = 842.0) -> int: ...
    def set_crop_box(self, page_index: int, rect: tuple[float, float, float, float]) -> None: ...
    def set_user_unit(self, page_index: int, unit: float) -> None: ...
    def attachments(self) -> list[Attachment]: ...
    def attachment_data(self, index: int) -> bytes: ...
    def add_attachment(
        self,
        name: str,
        data: bytes,
        *,
        description: str | None = None,
        mime_type: str | None = None,
        relationship: str | None = None,
        modified: str | None = None,
    ) -> None: ...
    def insert_text(
        self,
        page_index: int,
        point: tuple[float, float],
        text: str,
        *,
        font: str = "Helvetica",
        size: float = 11.0,
        color: tuple[float, float, float] = (0.0, 0.0, 0.0),
    ) -> int: ...
    def draw_line(
        self,
        page_index: int,
        start: tuple[float, float],
        end: tuple[float, float],
        *,
        width: float = 1.0,
        color: tuple[float, float, float] = (0.0, 0.0, 0.0),
    ) -> None: ...
    def draw_rect(
        self,
        page_index: int,
        rect: tuple[float, float, float, float],
        *,
        width: float = 1.0,
        color: tuple[float, float, float] | None = (0.0, 0.0, 0.0),
        fill: tuple[float, float, float] | None = None,
    ) -> None: ...
    def add_link(self, page_index: int, rect: tuple[float, float, float, float], uri: str) -> None: ...
    def set_outline(self, entries: list[tuple[int, str, int | None]]) -> None: ...
    def set_info(self, entries: dict[str, str]) -> None: ...
    def set_xmp(self, packet: bytes) -> None: ...
    def add_photo_pages(self, photos: list[bytes]) -> PhotoPages: ...
    def field_values(self) -> dict[str, list[str]]: ...
    def glyphs(self, page_index: int, annotations: bool = False) -> list[Glyph]: ...
    def render_png(
        self, page_index: int, scale: float, quarter_turns: int = 0, *, clip: tuple[int, int, int, int] | None = None
    ) -> bytes: ...
    def render_rgba(
        self, page_index: int, scale: float, quarter_turns: int = 0, *, clip: tuple[int, int, int, int] | None = None
    ) -> tuple[int, int, bytes]: ...
    def drawings(self, page_index: int, annotations: bool = False) -> Drawings: ...
    def outline(self) -> list[OutlineEntry]: ...
    def links(self, page_index: int) -> list[Link]: ...
    def words(self, page_index: int) -> list[TextWord]: ...
    def search(self, page_index: int, text: str) -> list[list[tuple[float, float, float, float]]]: ...
    def images(self, page_index: int) -> list[PageImage]: ...
    def extract_image(self, page_index: int, index: int) -> ExtractedImage: ...
    def page_labels(self) -> list[str] | None: ...
    def metadata(self) -> Metadata: ...
    def extract_pages(self, start: int, stop: int) -> bytes: ...
    def arrange(self, pages: list[int | tuple[int, int]]) -> bytes: ...
    def rotate_pages(self, turns: list[tuple[int, int]]) -> None: ...
    def insert_pages(self, at: int, pdf_contents: list[bytes], *, refuse_encrypted: bool = False) -> bytes: ...
    def own_page_boxes(self) -> int: ...
    def clear_flattened_form(self) -> int: ...
    def redact(
        self,
        page_index: int,
        areas: list[tuple[float, float, float, float]],
        *,
        images: str = "pixels",
        paths: str = "touched",
    ) -> Redacted: ...
    def apply_redactions(self, *, images: str = "pixels", paths: str = "touched") -> AppliedRedactions: ...
    def has_redaction_marks(self) -> bool: ...
    def rebuilt_for_redaction(self) -> bytes: ...
    def apply_xfdf(
        self,
        xfdf: str,
        *,
        flatten: bool = True,
        remove_annotations: bool = False,
        fonts: dict[str, bytes] | None = None,
        default_font: bytes | None = None,
    ) -> Applied: ...
    def convert_to_pdfa(self, *, level: str = "3a", language: str | None = None) -> PdfAReport: ...
    def check_pages(self) -> Peaks: ...

@final
class Timestamping:
    def __init__(
        self,
        client: Callable[[bytes], bytes] | Any,
        *,
        roots: list[bytes],
        intermediates: list[bytes] = ...,
        required: bool = True,
    ) -> None: ...
    @property
    def required(self) -> bool: ...

@final
class Signed:
    @property
    def pdf(self) -> bytes: ...
    @property
    def timestamp_time(self) -> int | None: ...
    @property
    def timestamp_authority(self) -> str | None: ...
    @property
    def timestamp_error(self) -> str | None: ...

def count(input_path: str | PathLike[str], *, limits: Limits | None = None) -> int: ...
def read_geometry(pdf_content: bytes, max_pages: int, *, limits: Limits | None = None) -> list[PageGeometry]: ...
def flatten_bytes(pdf_content: bytes, *, limits: Limits | None = None) -> bytes: ...
def apply_xfdf_bytes(
    pdf_content: bytes,
    xfdf_data: str,
    *,
    flatten: bool = True,
    remove_annotations: bool = False,
    fonts: dict[str, bytes] | None = None,
    default_font: bytes | None = None,
    limits: Limits | None = None,
) -> bytes: ...
def merge_bytes(
    pdf_contents: list[bytes], *, flatten: bool = True, limits: Limits | None = None, refuse_encrypted: bool = False
) -> bytes: ...
def grayscale(pdf_content: bytes, *, dpi: float = 200.0, limits: Limits | None = None) -> bytes: ...
def optimize(pdf_content: bytes, *, limits: Limits | None = None) -> bytes: ...
def release_cache() -> None: ...
def has_redaction_marks(pdf_content: bytes, *, limits: Limits | None = None) -> bool: ...
class StampMark(TypedDict):
    kind: Literal["stamp"]
    page_index: int
    rect: tuple[float, float, float, float]
    name: str
    png: bytes
    rotation: NotRequired[int]
    custom_data: NotRequired[dict[str, str]]

class FreeTextMark(TypedDict):
    kind: Literal["freetext"]
    page_index: int
    rect: tuple[float, float, float, float]
    name: str
    text: str
    rotation: NotRequired[int]
    size: NotRequired[float]
    color: NotRequired[tuple[float, float, float]]
    align: NotRequired[Literal["left", "center", "right"]]
    vertical_align: NotRequired[Literal["top", "center", "bottom"]]
    font: NotRequired[str]
    custom_data: NotRequired[dict[str, str]]

class InkMark(TypedDict):
    kind: Literal["ink"]
    page_index: int
    name: str
    strokes: list[list[tuple[float, float]]]
    color: NotRequired[tuple[float, float, float]]
    width: NotRequired[float]
    custom_data: NotRequired[dict[str, str]]

def xfdf_for(
    marks: list[StampMark | FreeTextMark | InkMark], *, deleted: list[tuple[int, str]] | None = None
) -> str: ...
def photos_to_pdf(photos: list[bytes], *, limits: Limits | None = None) -> tuple[bytes, list[int]]: ...
def docx_to_pdf(
    docx_content: bytes,
    *,
    font_dirs: list[str | PathLike[str]] | None = None,
    limits: Limits | None = None,
) -> tuple[bytes, DocxReport]: ...
def pdfa_convert(
    pdf_content: bytes, *, level: str = "3a", language: str | None = None, limits: Limits | None = None
) -> tuple[bytes, PdfAReport]: ...
def sign(
    pdf_content: bytes,
    signer: Any,
    *,
    field: str,
    certify: bool = False,
    reason: str | None = None,
    location: str | None = None,
    contact_info: str | None = None,
    signing_time: int | None = None,
    timestamp: Timestamping | None = None,
    allow_broken_signatures: bool = False,
) -> Signed: ...
def add_document_timestamp(pdf_content: bytes, timestamp: Timestamping, *, field: str = "DocTimeStamp1") -> Signed: ...
def verify_signatures(pdf_content: bytes) -> list[SignatureCheck]: ...
def add_validation_data(
    pdf_content: bytes,
    field: str,
    *,
    fetcher: Any,
    roots: list[bytes],
    intermediates: list[bytes] = ...,
    time: int | None = None,
) -> tuple[bytes, Coverage]: ...
