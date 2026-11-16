from collections.abc import Callable
from os import PathLike
from typing import Any, TypedDict, final

__version__: str

class PdfError(Exception): ...
class PdfReadError(PdfError): ...
class PdfPasswordError(PdfReadError): ...

class PdfLimitError(PdfReadError):
    limit: str

class PdfTimeoutError(PdfLimitError): ...
class XfdfError(PdfError): ...
class PdfAError(PdfError): ...
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
    characters_replaced: int
    deletions_ignored: int
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

class Metadata(TypedDict):
    version: str
    encrypted: bool
    linearized: bool
    has_forms: bool
    has_signatures: bool
    pdfa: str | None
    info: dict[str, str]

class Filled(TypedDict):
    occurrences: int
    characters_replaced: int

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
    ) -> Document: ...
    @staticmethod
    def open(
        path: str | PathLike[str],
        *,
        password: str | None = None,
        page_count: tuple[int, int] | None = None,
        limits: Limits | None = None,
    ) -> Document: ...
    @property
    def page_count(self) -> int: ...
    @property
    def is_modified(self) -> bool: ...
    def geometry(self) -> list[PageGeometry]: ...
    def save(self, *, xref: str = "stream") -> bytes: ...
    def save_incremental(self) -> bytes: ...
    def flatten(self, *, fields_only: bool = False) -> Flattened: ...
    def fields(self, page_index: int | None = None) -> list[Field]: ...
    def fill_fields(self, values: dict[str, str | bytes]) -> Filled: ...
    def field_values(self) -> dict[str, list[str]]: ...
    def glyphs(self, page_index: int, annotations: bool = False) -> list[Glyph]: ...
    def render_png(self, page_index: int, scale: float) -> bytes: ...
    def drawings(self, page_index: int, annotations: bool = False) -> Drawings: ...
    def outline(self) -> list[OutlineEntry]: ...
    def metadata(self) -> Metadata: ...
    def extract_pages(self, start: int, stop: int) -> bytes: ...
    def apply_xfdf(self, xfdf: str, *, flatten: bool = True) -> Applied: ...
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
    pdf_content: bytes, xfdf_data: str, *, flatten: bool = True, limits: Limits | None = None
) -> bytes: ...
def merge_bytes(pdf_contents: list[bytes], *, limits: Limits | None = None) -> bytes: ...
def grayscale(pdf_content: bytes, *, dpi: float = 200.0, limits: Limits | None = None) -> bytes: ...
def optimize(pdf_content: bytes, *, limits: Limits | None = None) -> bytes: ...
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
