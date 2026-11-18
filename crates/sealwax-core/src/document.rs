use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::fmt;
use std::ops::RangeInclusive;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};

use hayro_syntax::object::{
    Dict, MaybeRef, Object as HayroObject, ObjectIdentifier, Rect as HayroRect,
};
use hayro_syntax::page::{Page, Rotation as HayroRotation};
use hayro_syntax::xref::XRef;
use hayro_syntax::{LoadPdfError, Pdf};

use crate::error::{Error, Result};
use crate::geometry::{PageGeometry, Rect, Rotation};
use crate::limits::{Limits, Peaks, Walker, check_structure_streams};
use crate::object::{Converter, ObjId, Object, StreamData};
use crate::save::{self, SaveOptions};
use crate::trailer::BaseTrailer;

/// The highest object number sealwax gives a new object: the number of indirect objects PDF
/// readers commonly accept (ISO 32000-1, annex C). A file may define higher ones.
pub const MAX_OBJECT_NUMBER: u32 = 8_388_607;

/// Default ceiling on the size of a document handed to [`Document::open`].
const DEFAULT_MAX_FILE_BYTES: usize = 512 * 1024 * 1024;

/// The media box of a page that declares none, its own or inherited: US Letter, as PDFium and
/// other readers assume, where hayro would assume A4.
const LETTER: Rect = Rect {
    x1: 0.0,
    y1: 0.0,
    x2: 612.0,
    y2: 792.0,
};

/// How far up the page tree an inherited entry is looked for.
const MAX_TREE_DEPTH: usize = 32;

/// How to open a document.
#[derive(Debug, Clone)]
pub struct OpenOptions {
    password: String,
    refuse_encrypted: bool,
    page_count: Option<RangeInclusive<usize>>,
    max_file_bytes: usize,
    limits: Limits,
}

impl Default for OpenOptions {
    fn default() -> Self {
        Self {
            password: String::new(),
            refuse_encrypted: false,
            page_count: None,
            max_file_bytes: DEFAULT_MAX_FILE_BYTES,
            limits: Limits::default(),
        }
    }
}

impl OpenOptions {
    /// The user or owner password. Documents encrypted with an empty user password open without
    /// one, as they do in every viewer.
    pub fn password(mut self, password: impl Into<String>) -> Self {
        self.password = password.into();
        self
    }

    /// Refuse an encrypted document ([`Error::Encrypted`]), even one that opens without a
    /// password (an owner password only): sealwax writes documents unencrypted, so a copy of it
    /// would lose its protection.
    pub fn refuse_encrypted(mut self, refuse: bool) -> Self {
        self.refuse_encrypted = refuse;
        self
    }

    /// Whether encrypted documents are refused.
    pub fn refuses_encrypted(&self) -> bool {
        self.refuse_encrypted
    }

    /// Refuse a document whose page count falls outside `accepted`.
    pub fn page_count(mut self, accepted: RangeInclusive<usize>) -> Self {
        self.page_count = Some(accepted);
        self
    }

    /// Refuse data larger than `bytes`.
    pub fn max_file_bytes(mut self, bytes: usize) -> Self {
        self.max_file_bytes = bytes;
        self
    }

    /// What operations on the document may spend, and until when ([`Limits`]).
    pub fn limits(mut self, limits: Limits) -> Self {
        self.limits = limits;
        self
    }
}

/// An opened PDF document: the input, read lazily and never changed, plus the objects modified
/// or added since, which a save writes out.
///
/// Reading pages ([`Document::page_count`], [`Document::geometry`], [`Document::census`]) sees
/// the input only. Objects read with [`Document::object`] see the modifications.
pub struct Document {
    /// Shared, so that a reader can keep it alive with what it parsed from it.
    pub(crate) pdf: Arc<Pdf>,
    trailer: BaseTrailer,
    overlay: BTreeMap<ObjId, Object>,
    /// Each page object, and its index in hayro's page tree.
    page_index: HashMap<ObjId, usize>,
    /// The catalog as the pages were found from it, read when the document opened.
    catalog: Option<Object>,
    /// Pages added after those of the input ([`Document::append_pages`]), with their geometry.
    pub(crate) appended: Vec<(ObjId, PageGeometry)>,
    /// The rotation of input pages turned since ([`Document::rotate_pages`]), by index.
    rotations: HashMap<usize, Rotation>,
    /// The crop box of input pages given one since ([`Document::set_crop_box`]), by index.
    crop_boxes: HashMap<usize, Rect>,
    info: Option<ObjId>,
    next_number: u32,
    /// The highest object number the file defines, read from its bytes when first needed.
    highest_defined: OnceLock<u32>,
    geometry: OnceLock<Vec<PageGeometry>>,
    pub(crate) limits: Limits,
    /// The pages [`Document::check_page`] found within the limits: neither the input nor the
    /// limits change, so a page is walked once.
    checked: Mutex<HashSet<usize>>,
    /// The bytes each stream counted by [`Document::check_page`] decodes to: the font programs
    /// pages share are decoded once.
    streams: Mutex<HashMap<ObjectIdentifier, u64>>,
    /// The document as it stands, saved and read again, while nothing changes it
    /// ([`Document::reopened_if_modified`]).
    view: Mutex<Option<Arc<Document>>>,
    /// Content was taken out ([`Document::mark_redacted`]).
    redacted: bool,
}

impl fmt::Debug for Document {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Document")
            .field("page_count", &self.page_count())
            .finish_non_exhaustive()
    }
}

impl Document {
    /// Open a document from its bytes.
    ///
    /// # Errors
    /// [`Error::PasswordRequired`] when the password does not open it, [`Error::Malformed`] when
    /// it cannot be read even after repair, [`Error::PagesMissing`] when it declares more pages
    /// than can be read, [`Error::PageCountOutOfRange`] or [`Error::LimitExceeded`] when it falls
    /// outside the options' bounds.
    pub fn open(data: impl Into<Vec<u8>>, options: &OpenOptions) -> Result<Self> {
        let data: Vec<u8> = data.into();
        options.limits.check_time()?;
        if data.len() > options.max_file_bytes {
            return Err(Error::LimitExceeded {
                what: "max_file_bytes",
            });
        }
        // hayro decodes these whole as it opens the document.
        check_structure_streams(&data, &options.limits)?;
        let pdf = Pdf::new_with_password(data, &options.password).map_err(|error| match error {
            LoadPdfError::Decryption(_) => Error::PasswordRequired,
            LoadPdfError::Invalid => Error::Malformed,
        })?;
        let root = document_root(&pdf);
        let trailer = BaseTrailer::read(pdf.data().as_ref(), root);
        if options.refuse_encrypted && trailer.encrypted() {
            return Err(Error::Encrypted);
        }
        let mut document = Self {
            info: trailer.info(),
            next_number: trailer.size().unwrap_or(1).max(1),
            trailer,
            overlay: BTreeMap::new(),
            page_index: HashMap::new(),
            catalog: None,
            appended: Vec::new(),
            rotations: HashMap::new(),
            crop_boxes: HashMap::new(),
            highest_defined: OnceLock::new(),
            geometry: OnceLock::new(),
            checked: Mutex::new(HashSet::new()),
            streams: Mutex::new(HashMap::new()),
            view: Mutex::new(None),
            redacted: false,
            limits: options.limits,
            pdf: Arc::new(pdf),
        };
        // The first page to claim an object number keeps it.
        let mut page_index = HashMap::new();
        for (index, id) in document.page_ids().into_iter().enumerate() {
            if let Some(id) = id {
                page_index.entry(id).or_insert(index);
            }
        }
        document.page_index = page_index;
        // Measured now, before any other read: once hayro has repaired a damaged
        // cross-reference, an ancestor of a page can resolve to another object (hayro issue
        // #1746), and the geometry must stay what the page tree said when the document opened.
        let _ = document.geometry();
        // The catalog too: read later, a damaged file can resolve it to another definition
        // (hayro issue #1746), whose `/Pages` leads to other pages than those counted.
        document.catalog = document
            .load(document.catalog_id(), StreamData::Skip)
            .ok()
            .flatten();
        let count = document.page_count();
        // hayro skips a page it cannot read. A document that has lost pages is refused
        // rather than signed short. A /Count lower than the pages found is only a wrong count.
        if let Some(declared) = document.declared_page_count()
            && declared > i64::try_from(count).unwrap_or(i64::MAX)
        {
            return Err(Error::PagesMissing {
                found: count,
                declared,
            });
        }
        if let Some(accepted) = &options.page_count
            && !accepted.contains(&count)
        {
            return Err(Error::PageCountOutOfRange {
                count,
                accepted: accepted.clone(),
            });
        }
        document.limits.check_time()?;
        Ok(document)
    }

    /// Open the document stored at `path`.
    ///
    /// # Errors
    /// [`Error::Io`] when the file cannot be read, then as [`Document::open`].
    pub fn open_path(path: impl AsRef<Path>, options: &OpenOptions) -> Result<Self> {
        Self::open(std::fs::read(path)?, options)
    }

    /// Number of pages: those of the input, then those appended.
    pub fn page_count(&self) -> usize {
        self.pdf.pages().len() + self.appended.len()
    }

    /// The page count the root of the page tree declares (`/Count`), when it is a number.
    ///
    /// It can disagree with [`Document::page_count`], the pages actually found: in a damaged
    /// file, either the count lies or a page could not be read.
    pub fn declared_page_count(&self) -> Option<i64> {
        let xref = self.pdf.xref();
        xref.get::<Dict<'_>>(xref.root_id())?
            .get::<Dict<'_>>(b"Pages")?
            .get::<i64>(b"Count")
    }

    /// What operations on the document may spend, and until when.
    pub fn limits(&self) -> &Limits {
        &self.limits
    }

    /// [`Error::Timeout`] once the deadline of [`Document::limits`] has passed: what a long
    /// operation calls between two steps.
    ///
    /// # Errors
    /// [`Error::Timeout`].
    /// Whether the document is encrypted (its trailer names an `/Encrypt` dictionary): its
    /// strings are read decrypted, but for the signature values, which are not encrypted.
    #[must_use]
    pub fn is_encrypted(&self) -> bool {
        self.trailer.encrypted()
    }

    pub fn check_time(&self) -> Result<()> {
        self.limits.check_time()
    }

    /// Check that interpreting the pages of the input (rendering, tagging) stays within
    /// [`Document::limits`], without interpreting them: every content stream drawing them reads
    /// is walked once, and what drawing each page costs is added up, each form counted each time
    /// it is drawn. Also bounded: what the interpreter builds from images, mesh shadings and CID
    /// font widths. The interpreter has no bounds of its own.
    ///
    /// Returns the most a page spent on each bound.
    ///
    /// # Errors
    /// [`Error::LimitExceeded`] naming the bound passed, [`Error::Timeout`].
    pub fn check_pages(&self) -> Result<Peaks> {
        self.limits.check_time()?;
        let mut walker = Walker::new(self.limits);
        for page in self.pdf.pages().iter() {
            walker.page(page)?;
            self.limits.check_time()?;
        }
        Ok(walker.peaks)
    }

    /// Check that interpreting page `page_index` (0-based), its annotations' appearances included,
    /// stays within [`Document::limits`], as [`Document::check_pages`] checks every page: for a
    /// caller that draws one page. A page found within them is not walked again; the deadline
    /// is checked every time.
    ///
    /// # Errors
    /// [`Error::LimitExceeded`] naming the bound passed, [`Error::Timeout`],
    /// [`Error::InvalidArgument`] for a page the document does not have.
    pub fn check_page(&self, page_index: usize) -> Result<()> {
        self.limits.check_time()?;
        if self
            .checked
            .lock()
            .is_ok_and(|checked| checked.contains(&page_index))
        {
            return Ok(());
        }
        let pages = self.pdf.pages();
        let page = pages
            .get(page_index)
            .ok_or(Error::InvalidArgument { what: "page index" })?;
        let mut walker = Walker::new(self.limits);
        if let Ok(streams) = self.streams.lock() {
            walker.streams.clone_from(&streams);
        }
        walker.page(page)?;
        if let Ok(mut streams) = self.streams.lock() {
            streams.extend(walker.streams);
        }
        if let Ok(mut checked) = self.checked.lock() {
            checked.insert(page_index);
        }
        Ok(())
    }

    /// Check that interpreting `content` with the resources of page `page_index` (0-based) stays
    /// within [`Document::limits`], as [`Document::check_pages`] checks a page's own content: for
    /// a caller that draws content of its own over a page.
    ///
    /// # Errors
    /// [`Error::LimitExceeded`] naming the bound passed, [`Error::Timeout`],
    /// [`Error::InvalidArgument`] for a page the document does not have.
    pub fn check_content(&self, page_index: usize, content: &[u8]) -> Result<()> {
        self.limits.check_time()?;
        let pages = self.pdf.pages();
        let page = pages
            .get(page_index)
            .ok_or(Error::InvalidArgument { what: "page index" })?;
        Walker::new(self.limits).content_of(content, page.resources())?;
        Ok(())
    }

    /// The most interpreting a page of the input would spend on each bound of the limits, measured
    /// as [`Document::check_pages`] does but without bounds, but the deadline and
    /// [`Limits::max_stream_bytes`]: what calibrates them.
    ///
    /// # Errors
    /// [`Error::Timeout`], [`Error::LimitExceeded`] for a stream past its bound.
    pub fn measure_pages(&self) -> Result<Peaks> {
        // Streams stay bounded: measuring must not decode what the limits would refuse.
        let mut walker = Walker::new(Limits {
            deadline: self.limits.deadline,
            max_stream_bytes: self.limits.max_stream_bytes,
            ..Limits::unbounded()
        });
        self.limits.check_time()?;
        for page in self.pdf.pages().iter() {
            walker.page(page)?;
            self.limits.check_time()?;
        }
        Ok(walker.peaks)
    }

    /// The document catalog (`/Root`).
    pub fn catalog_id(&self) -> ObjId {
        document_root(&self.pdf)
    }

    /// The page object of each page, in page order: those of the input as read from it, then
    /// those appended.
    pub fn page_ids(&self) -> Vec<Option<ObjId>> {
        self.pdf
            .pages()
            .iter()
            .map(|page| {
                let id = page.raw().obj_id()?;
                Some(ObjId {
                    number: id.obj_number,
                    generation: u16::try_from(id.gen_number).ok()?,
                })
            })
            .chain(self.appended.iter().map(|(id, _)| Some(*id)))
            .collect()
    }

    /// The document information dictionary (`/Info`), if any.
    pub fn info_id(&self) -> Option<ObjId> {
        self.info
    }

    /// Set or remove the document information dictionary.
    pub fn set_info_id(&mut self, info: Option<ObjId>) {
        self.info = info;
    }

    /// An indirect object, modified if it was, as an owned copy; `None` when it does not exist.
    ///
    /// # Errors
    /// [`Error::LimitExceeded`] when the object nests deeper than sealwax accepts.
    pub fn object(&self, id: ObjId) -> Result<Option<Object>> {
        self.load(id, StreamData::Copy)
    }

    /// An indirect object as [`Document::object`] returns it, but a stream of the input comes
    /// without its data: to read dictionaries without copying what streams hold.
    ///
    /// # Errors
    /// [`Error::LimitExceeded`] when the object nests deeper than sealwax accepts.
    pub fn object_without_data(&self, id: ObjId) -> Result<Option<Object>> {
        self.load(id, StreamData::Skip)
    }

    /// The objects reachable from the catalog and the document information dictionary, modified
    /// and added ones included, breadth first: what a full save writes.
    ///
    /// # Errors
    /// [`Error::LimitExceeded`] when an object nests deeper than sealwax accepts.
    pub fn reachable_objects(&self) -> Result<Vec<ObjId>> {
        let roots: Vec<ObjId> = [Some(self.catalog_id()), self.info]
            .into_iter()
            .flatten()
            .collect();
        let mut order = Vec::new();
        let mut seen: HashSet<ObjId> = roots.iter().copied().collect();
        let mut queue: VecDeque<ObjId> = roots.into_iter().collect();
        while let Some(id) = queue.pop_front() {
            if order.len() % 1024 == 1023 {
                self.check_time()?;
            }
            let Some(object) = self.load(id, StreamData::Skip)? else {
                continue;
            };
            order.push(id);
            save::visit_refs(&object, &mut |reference| {
                if seen.insert(reference) {
                    queue.push_back(reference);
                }
            });
        }
        Ok(order)
    }

    /// Whether an object was replaced or added since the document was opened. Reading pages does
    /// not see such changes: a process that reads them (a PDF/A conversion) needs the document
    /// saved and opened again.
    pub fn is_modified(&self) -> bool {
        !self.overlay.is_empty()
    }

    /// A new document of pages `pages` of this one (from 0, the end excluded; at least one),
    /// each with everything it reaches (contents, resources, annotations and their links), as
    /// [`Document::append_pages`] copies them: the outlines, the form and the structure tree are
    /// left behind, and a link to a page left out leads nowhere.
    ///
    /// # Errors
    /// [`Error::InvalidArgument`] for an empty range or pages this document does not have; as
    /// [`Document::append_pages`].
    pub fn extract_pages(&self, pages: std::ops::Range<usize>) -> Result<Document> {
        if pages.is_empty() || pages.end > self.page_count() {
            return Err(Error::InvalidArgument { what: "page range" });
        }
        let mut out = Document::empty()?;
        out.limits = self.limits;
        out.append_page_range(self, pages)?;
        Ok(out)
    }

    /// The document as it stands, for what reads pages: `None` when nothing was modified (read
    /// this one), else its full save opened again, with the same limits. The copy is kept until
    /// the document changes again: reading many pages of a modified document costs one save.
    ///
    /// # Errors
    /// As [`Document::save`] and [`Document::open`].
    pub fn reopened_if_modified(&self) -> Result<Option<Arc<Document>>> {
        if !self.is_modified() {
            return Ok(None);
        }
        let mut view = self
            .view
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(view) = view.as_ref() {
            return Ok(Some(Arc::clone(view)));
        }
        let saved = self.save(&SaveOptions::default())?;
        // Already in memory: the bound on the input's size is not this one's.
        let options = OpenOptions::default()
            .limits(self.limits)
            .max_file_bytes(usize::MAX);
        let reopened = Arc::new(Document::open(saved, &options)?);
        *view = Some(Arc::clone(&reopened));
        Ok(Some(reopened))
    }

    /// The document as it stands, its incremental save opened again: its objects keep their
    /// numbers, for what reads its pages before changing them (`sealwax-text`'s redaction). Made
    /// even when it is redacted: the copy is marked so as well. Not a stable interface.
    ///
    /// # Errors
    /// As [`Document::save_incremental`] and [`Document::open`].
    #[doc(hidden)]
    pub fn reopened_keeping_numbers(&self) -> Result<Document> {
        let options = OpenOptions::default()
            .limits(self.limits)
            .max_file_bytes(usize::MAX);
        let mut reopened = Document::open(save::incremental(self)?, &options)?;
        reopened.redacted = self.redacted;
        Ok(reopened)
    }

    /// Mark content as taken out of the document (redacted): [`Document::save_incremental`]
    /// refuses it from then on, since the bytes it was opened from, which an incremental save
    /// keeps, still hold that content. Save it whole instead. `sealwax_text::redact` marks the
    /// documents it changes.
    pub fn mark_redacted(&mut self) {
        self.redacted = true;
    }

    /// Whether content was taken out of the document since it was opened
    /// ([`Document::mark_redacted`]).
    pub fn is_redacted(&self) -> bool {
        self.redacted
    }

    /// Replace an indirect object, or create it under that identifier.
    pub fn set_object(&mut self, id: ObjId, object: Object) {
        self.next_number = self.next_number.max(id.number.saturating_add(1));
        self.overlay.insert(id, object);
        // What was read again no longer stands.
        if let Ok(view) = self.view.get_mut() {
            *view = None;
        }
    }

    /// Add an indirect object under a new number, and return its identifier.
    ///
    /// # Errors
    /// [`Error::LimitExceeded`] when no object number up to [`MAX_OBJECT_NUMBER`] is free,
    /// [`Error::Timeout`] past the deadline of the document's limits.
    pub fn add_object(&mut self, object: Object) -> Result<ObjId> {
        let id = ObjId::new(self.next_free_number()?);
        self.set_object(id, object);
        Ok(id)
    }

    /// Save in full: everything reachable is rewritten, decrypted, renumbered.
    ///
    /// # Errors
    /// [`Error::Unsupported`] for a document without pages, [`Error::Malformed`] when the
    /// catalog cannot be read, [`Error::LimitExceeded`] when an object nests deeper than sealwax
    /// accepts, [`Error::SaveCheck`] when the output does not read back.
    pub fn save(&self, options: &SaveOptions) -> Result<Vec<u8>> {
        save::full(self, options)
    }

    /// Save as an incremental update: the input bytes unchanged, followed by the modified and
    /// added objects. The input is returned as is when nothing changed.
    ///
    /// # Errors
    /// [`Error::Unsupported`] when the input is encrypted or its cross-reference had to be
    /// repaired: save it in full first, which sealwax always does before signing. The same when
    /// content was redacted ([`Document::mark_redacted`]): the input would keep it.
    /// [`Error::SaveCheck`] when the output does not read back.
    pub fn save_incremental(&self) -> Result<Vec<u8>> {
        if self.redacted {
            return Err(Error::Unsupported {
                what: "an incremental save of a redacted document, which would keep what was \
                       taken out: save it whole",
            });
        }
        save::incremental(self)
    }

    /// The hayro-syntax document underneath, for the other crates of the workspace that read
    /// content with hayro (`sealwax-text`). Not a stable interface.
    #[doc(hidden)]
    pub fn hayro(&self) -> &Pdf {
        &self.pdf
    }

    /// The hayro-syntax document underneath, shared: for a reader that keeps what it parsed
    /// from it (fonts) from one call to the next. Not a stable interface.
    #[doc(hidden)]
    pub fn hayro_shared(&self) -> Arc<Pdf> {
        Arc::clone(&self.pdf)
    }

    pub(crate) fn data(&self) -> &[u8] {
        self.pdf.data().as_ref()
    }

    pub(crate) fn pdf(&self) -> &Pdf {
        &self.pdf
    }

    pub(crate) fn base_trailer(&self) -> &BaseTrailer {
        &self.trailer
    }

    pub(crate) fn overlay(&self) -> &BTreeMap<ObjId, Object> {
        &self.overlay
    }

    /// A number used neither by the input nor by the overlay: the lowest from `next_number`, else,
    /// past [`MAX_OBJECT_NUMBER`], the lowest from 1. The input's `/Size` can be wrong or
    /// unreadable, and an object hayro cannot read whole can still be a page it read as a
    /// dictionary: numbers start above every object the file defines, and skip whatever reads.
    pub(crate) fn next_free_number(&self) -> Result<u32> {
        let highest = *self
            .highest_defined
            .get_or_init(|| highest_defined(self.pdf.data().as_ref()));
        let start = self.next_number.max(highest.saturating_add(1)).max(1);
        let xref = self.pdf.xref();
        let used = |number: u32| {
            let id = ObjId::new(number);
            self.overlay.contains_key(&id)
                || self.page_index.contains_key(&id)
                || xref.get::<HayroObject<'_>>(id.to_hayro()).is_some()
                || xref.get::<Dict<'_>>(id.to_hayro()).is_some()
        };
        let upward = start..=MAX_OBJECT_NUMBER;
        let below = 1..start.min(MAX_OBJECT_NUMBER.saturating_add(1));
        for (tried, number) in upward.chain(below).enumerate() {
            if tried % 4096 == 4095 {
                self.limits.check_time()?;
            }
            if !used(number) {
                return Ok(number);
            }
        }
        Err(Error::LimitExceeded {
            what: "object numbers",
        })
    }

    /// For each page without its own `/Resources`, those of its nearest ancestor that has some.
    pub(crate) fn inherited_resources(&self) -> Result<Vec<Option<Object>>> {
        let converter = Converter {
            decrypted: self.trailer.encrypted(),
            stream_data: StreamData::Skip,
        };
        // What each node of the page tree hands down, resolved once: a node with many kids is a
        // large dictionary, read again for each kid otherwise.
        let mut handed_down: HashMap<ObjectIdentifier, Option<Object>> = HashMap::new();
        self.pdf
            .pages()
            .iter()
            .map(|page| {
                let raw = page.raw();
                if raw.contains_key(b"Resources") {
                    return Ok(None);
                }
                let mut unknown = Vec::new();
                let mut handed = None;
                let mut parent = raw.get_raw::<Dict<'_>>(b"Parent");
                for _ in 0..MAX_TREE_DEPTH {
                    let Some(node) = parent_node(
                        parent,
                        self.pdf.xref(),
                        |id| handed_down.get(&id).cloned(),
                        &mut handed,
                    ) else {
                        break;
                    };
                    if let Some(value) = node.get_raw::<HayroObject<'_>>(b"Resources") {
                        handed = Some(converter.value(value)?);
                        unknown.push(node);
                        break;
                    }
                    parent = node.get_raw::<Dict<'_>>(b"Parent");
                    unknown.push(node);
                }
                for node in unknown {
                    if let Some(id) = node.obj_id() {
                        handed_down.insert(id, handed.clone());
                    }
                }
                Ok(handed)
            })
            // Appended pages carry what they inherited.
            .chain(self.appended.iter().map(|_| Ok(None)))
            .collect()
    }

    pub(crate) fn load(&self, id: ObjId, stream_data: StreamData) -> Result<Option<Object>> {
        if let Some(object) = self.overlay.get(&id) {
            return Ok(Some(object.clone()));
        }
        if id == self.catalog_id()
            && let Some(catalog) = &self.catalog
        {
            return Ok(Some(catalog.clone()));
        }
        let converter = Converter {
            decrypted: self.trailer.encrypted(),
            stream_data,
        };
        // A page object is the dictionary hayro's page tree read when the document was opened.
        // Read again, it can resolve to another object: once hayro has repaired a damaged
        // cross-reference, an object defined twice is the later definition (hayro issue #1746),
        // and the pages the document counts and measures would no longer be the pages it saves.
        if let Some(page) = self
            .page_index
            .get(&id)
            .and_then(|index| self.pdf.pages().get(*index))
        {
            return converter
                .dict_of(page.raw())
                .map(|dict| Some(Object::Dict(dict)));
        }
        let xref = self.pdf.xref();
        // Read as a dictionary, hayro accepts some damage it refuses when reading any object.
        xref.get::<HayroObject<'_>>(id.to_hayro())
            .or_else(|| xref.get::<Dict<'_>>(id.to_hayro()).map(HayroObject::Dict))
            .map(|object| converter.indirect(object))
            .transpose()
    }

    /// The geometry of every page, in page order.
    ///
    /// Computed once: reading a damaged file can resolve the same object differently once hayro
    /// has repaired its cross-reference (hayro issue #1746), and a document must not change its
    /// geometry from one call to the next, nor between what it reports and what it saves.
    pub fn geometry(&self) -> Vec<PageGeometry> {
        let mut geometry = self.input_geometry().clone();
        for (index, rotation) in &self.rotations {
            if let Some(page) = geometry.get_mut(*index) {
                page.rotation = *rotation;
            }
        }
        for (index, crop_box) in &self.crop_boxes {
            if let Some(page) = geometry.get_mut(*index) {
                page.crop_box = *crop_box;
            }
        }
        let first = geometry.len();
        geometry.extend(
            self.appended
                .iter()
                .enumerate()
                .map(|(index, (_, page))| PageGeometry {
                    page_number: first + index + 1,
                    ..*page
                }),
        );
        geometry
    }

    /// Turn pages where they are, by quarter turns clockwise added to their rotation
    /// (`/Rotate`, written on the page itself when it inherits it): the document keeps all it
    /// has (outline, structure, information, metadata), unlike [`Document::arrange`], which makes
    /// a new one. A page named twice turns twice.
    ///
    /// # Errors
    /// [`Error::InvalidArgument`] for a page the document does not have, nothing turned then.
    pub fn rotate_pages(&mut self, turns: &[(usize, i32)]) -> Result<()> {
        let ids = self.page_ids();
        if turns
            .iter()
            .any(|(index, _)| !matches!(ids.get(*index), Some(Some(_))))
        {
            return Err(Error::InvalidArgument { what: "page index" });
        }
        let mut geometry = self.geometry();
        for (index, quarter_turns) in turns {
            let Some(Some(id)) = ids.get(*index).copied() else {
                continue;
            };
            let rotation = geometry[*index].rotation.turned(*quarter_turns);
            geometry[*index].rotation = rotation;
            let Some(Object::Dict(mut page)) = self.object(id)? else {
                continue;
            };
            page.insert("Rotate", i64::from(rotation.degrees()));
            self.set_object(id, Object::Dict(page));
            match self.page_index.get(&id) {
                Some(input) => {
                    self.rotations.insert(*input, rotation);
                }
                None => {
                    if let Some((_, appended)) = self.appended.iter_mut().find(|(p, _)| *p == id) {
                        appended.rotation = rotation;
                    }
                }
            }
        }
        Ok(())
    }

    /// Give page `page_index` (from 0) the crop box `crop_box`, in its default user space: what
    /// a reader shows of it, the media box's part it covers (a reader clips it so).
    ///
    /// # Errors
    /// [`Error::InvalidArgument`] for a page the document does not have, or a crop box that
    /// leaves nothing of the media box.
    pub fn set_crop_box(&mut self, page_index: usize, crop_box: Rect) -> Result<()> {
        let geometry = self.geometry();
        let (Some(Some(id)), Some(page)) = (
            self.page_ids().get(page_index).copied(),
            geometry.get(page_index),
        ) else {
            return Err(Error::InvalidArgument { what: "page index" });
        };
        let clipped = crop_box.intersect(&page.media_box);
        let values = [crop_box.x1, crop_box.y1, crop_box.x2, crop_box.y2];
        if !values.iter().all(|v| v.is_finite())
            || clipped.width() <= 0.0
            || clipped.height() <= 0.0
        {
            return Err(Error::InvalidArgument { what: "crop box" });
        }
        let Some(Object::Dict(mut dict)) = self.object(id)? else {
            return Err(Error::InvalidArgument { what: "page index" });
        };
        dict.insert(
            "CropBox",
            vec![
                clipped.x1.into(),
                clipped.y1.into(),
                clipped.x2.into(),
                clipped.y2.into(),
            ],
        );
        self.set_object(id, Object::Dict(dict));
        match self.page_index.get(&id) {
            Some(input) => {
                self.crop_boxes.insert(*input, clipped);
            }
            None => {
                if let Some((_, appended)) = self.appended.iter_mut().find(|(p, _)| *p == id) {
                    appended.crop_box = clipped;
                }
            }
        }
        Ok(())
    }

    /// The geometry of the page whose object is `id`, as [`Document::geometry`] reports it,
    /// without measuring every page: `None` when `id` is no page of the document.
    pub fn page_geometry(&self, id: ObjId) -> Option<PageGeometry> {
        let input = self.input_geometry();
        if let Some(index) = self.page_index.get(&id) {
            return input.get(*index).map(|page| PageGeometry {
                rotation: self.rotations.get(index).copied().unwrap_or(page.rotation),
                crop_box: self.crop_boxes.get(index).copied().unwrap_or(page.crop_box),
                ..*page
            });
        }
        let (index, (_, page)) = self
            .appended
            .iter()
            .enumerate()
            .find(|(_, (page, _))| *page == id)?;
        Some(PageGeometry {
            page_number: input.len() + index + 1,
            ..*page
        })
    }

    /// The geometry of the pages of the input, measured once.
    fn input_geometry(&self) -> &Vec<PageGeometry> {
        self.geometry.get_or_init(|| {
            let mut inherited = HashMap::new();
            self.pdf
                .pages()
                .iter()
                .enumerate()
                .map(|(index, page)| {
                    page_geometry(index + 1, page, self.pdf.xref(), &mut inherited)
                })
                .collect()
        })
    }
}

/// The highest object number defined in `data` by an `N G obj` header, whatever the
/// cross-reference says. A header in a comment (after a `%` on its line) is none, and a number
/// past [`MAX_OBJECT_NUMBER`] is not counted: new objects take free numbers below it.
fn highest_defined(data: &[u8]) -> u32 {
    let mut highest = 0;
    let mut at = 0;
    while at + 3 <= data.len() {
        if &data[at..at + 3] != b"obj" {
            at += 1;
            continue;
        }
        let mut i = at;
        let mut fields: [Option<u64>; 2] = [None, None];
        for field in &mut fields {
            let space_end = i;
            while i > 0 && data[i - 1].is_ascii_whitespace() {
                i -= 1;
            }
            if i == space_end {
                break;
            }
            let digits_end = i;
            while i > 0 && data[i - 1].is_ascii_digit() && digits_end - i < 10 {
                i -= 1;
            }
            if i == digits_end {
                break;
            }
            *field = std::str::from_utf8(&data[i..digits_end])
                .ok()
                .and_then(|digits| digits.parse().ok());
        }
        let line_start = data[..i]
            .iter()
            .rposition(|b| *b == b'\n' || *b == b'\r')
            .map_or(0, |p| p + 1);
        let in_comment = data[line_start..i].contains(&b'%');
        if let [Some(_), Some(number)] = fields
            && !in_comment
            && let Ok(number) = u32::try_from(number)
            && number <= MAX_OBJECT_NUMBER
        {
            highest = highest.max(number);
        }
        at += 3;
    }
    highest
}

fn document_root(pdf: &Pdf) -> ObjId {
    let root = pdf.xref().root_id();
    ObjId {
        number: root.obj_number,
        generation: u16::try_from(root.gen_number).unwrap_or(0),
    }
}

/// The media and crop boxes a node of the page tree hands down: its own, else its parent's.
pub(crate) type Boxes = (Option<HayroRect>, Option<HayroRect>);

/// `page_number`'s geometry. `inherited` holds what each node of the page tree hands down,
/// resolved once: a node with many kids is a large dictionary, read again for each kid
/// otherwise.
pub(crate) fn page_geometry(
    page_number: usize,
    page: &Page<'_>,
    xref: &XRef,
    inherited: &mut HashMap<ObjectIdentifier, Boxes>,
) -> PageGeometry {
    let raw = page.raw();
    // Both boxes from the same walk up the page tree, never mixed with hayro's own page boxes:
    // in a damaged file the two can resolve the parent differently, and hayro then falls back
    // on A4 where the walk found a box.
    let (media_box, crop_box) = page_boxes(raw, xref, inherited);
    let media_box = media_box.map_or(LETTER, rect);
    let crop_box = crop_box.map_or(media_box, rect);
    let mut crop_box = crop_box.intersect(&media_box);
    // A crop box that leaves nothing visible (zero area, or outside the media box) is ignored, as
    // pdf.js does: the page shows its whole media box.
    if crop_box.width() <= 0.0 || crop_box.height() <= 0.0 {
        crop_box = media_box;
    }
    PageGeometry {
        page_number,
        media_box,
        crop_box,
        rotation: match page.rotation() {
            HayroRotation::None => Rotation::None,
            HayroRotation::Horizontal => Rotation::Quarter,
            HayroRotation::Flipped => Rotation::Half,
            HayroRotation::FlippedHorizontal => Rotation::ThreeQuarters,
        },
        // UserUnit is not inheritable (ISO 32000-1, table 30).
        user_unit: page
            .raw()
            .get::<f64>(b"UserUnit")
            .filter(|unit| unit.is_finite() && *unit > 0.0)
            .unwrap_or(1.0),
    }
}

/// The media and crop boxes of the page or, for each it lacks, of its nearest ancestor that has
/// one, at most [`MAX_TREE_DEPTH`] levels up.
pub(crate) fn page_boxes(
    page: &Dict<'_>,
    xref: &XRef,
    inherited: &mut HashMap<ObjectIdentifier, Boxes>,
) -> Boxes {
    // The ancestors whose boxes are not known yet, nearest first, and what the first known one
    // hands down.
    let mut unknown = Vec::new();
    let mut handed: Boxes = (None, None);
    let mut parent = page.get_raw::<Dict<'_>>(b"Parent");
    for _ in 1..MAX_TREE_DEPTH {
        let Some(node) = parent_node(parent, xref, |id| inherited.get(&id).copied(), &mut handed)
        else {
            break;
        };
        parent = node.get_raw::<Dict<'_>>(b"Parent");
        unknown.push(node);
    }
    for node in unknown.iter().rev() {
        handed = (
            node.get::<HayroRect>(b"MediaBox").or(handed.0),
            node.get::<HayroRect>(b"CropBox").or(handed.1),
        );
        if let Some(id) = node.obj_id() {
            inherited.insert(id, handed);
        }
    }
    (
        page.get::<HayroRect>(b"MediaBox").or(handed.0),
        page.get::<HayroRect>(b"CropBox").or(handed.1),
    )
}

/// The node `parent` names, read, unless what it hands down is already `known`: then that goes
/// into `handed` and there is no node to read. Only an unknown node is read: a node with many
/// kids is a large dictionary.
pub(crate) fn parent_node<'a, T>(
    parent: Option<MaybeRef<Dict<'a>>>,
    xref: &'a XRef,
    known: impl FnOnce(ObjectIdentifier) -> Option<T>,
    handed: &mut T,
) -> Option<Dict<'a>> {
    match parent? {
        MaybeRef::Ref(reference) => {
            let id = ObjectIdentifier::from(reference);
            if let Some(found) = known(id) {
                *handed = found;
                return None;
            }
            xref.get::<Dict<'a>>(id)
        }
        MaybeRef::NotRef(node) => Some(node),
    }
}

fn rect(r: HayroRect) -> Rect {
    Rect::from_corners(r.x0, r.y0, r.x1, r.y1)
}
