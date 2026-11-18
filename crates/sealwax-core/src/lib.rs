//! Core of sealwax: opening PDF documents, their pages and the geometry each page declares.
//!
//! Reading is delegated to [`hayro_syntax`], a robust read-only parser. Modified and added
//! objects live in an overlay of owned [`Object`]s, written out by a full save
//! ([`Document::save`]) or an incremental one ([`Document::save_incremental`]).
//!
//! ```no_run
//! use sealwax_core::{Document, OpenOptions};
//!
//! let bytes = std::fs::read("in.pdf")?;
//! let doc = Document::open(bytes, &OpenOptions::default().page_count(1..=1000))?;
//! for geometry in doc.geometry() {
//!     println!("{geometry:?}");
//! }
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

mod attachments;
mod build;
mod census;
mod decode;
mod document;
mod encrypt;
mod error;
mod flattened;
mod geometry;
mod limits;
mod links;
mod merge;
mod metadata;
mod object;
mod optimize;
mod outline;
mod save;
mod serialize;
mod text;
mod trailer;

pub use attachments::{Attachment, NewAttachment, is_mime_type, mime_type_for};
pub use build::NewOutlineEntry;
pub use census::{Census, Font, Form, PageFacts, Tagging};
pub use document::{Document, MAX_OBJECT_NUMBER, OpenOptions};
pub use encrypt::{Cipher, Encrypt, PERMISSIONS};
pub use error::{Error, Result};
pub use geometry::{PageGeometry, Rect, Rotation};
pub use limits::{Limits, Peaks};
pub use links::{Link, LinkTarget};
pub use merge::PagePick;
pub use metadata::{Encryption, Metadata};
pub use object::{Dict, Name, ObjId, Object, Stream};
pub use optimize::{Kept, Optimized, optimize};
pub use outline::OutlineEntry;
pub use save::{SaveOptions, XrefFormat};
pub use text::{decode_text_string, encode_text_string, is_standard_text_string};
