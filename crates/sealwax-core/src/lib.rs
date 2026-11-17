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

mod census;
mod decode;
mod document;
mod error;
mod flattened;
mod geometry;
mod limits;
mod merge;
mod metadata;
mod object;
mod optimize;
mod outline;
mod save;
mod serialize;
mod text;
mod trailer;

pub use census::{Census, Font, Form, PageFacts, Tagging};
pub use document::{Document, MAX_OBJECT_NUMBER, OpenOptions};
pub use error::{Error, Result};
pub use geometry::{PageGeometry, Rect, Rotation};
pub use limits::{Limits, Peaks};
pub use merge::PagePick;
pub use metadata::Metadata;
pub use object::{Dict, Name, ObjId, Object, Stream};
pub use optimize::{Kept, Optimized, optimize};
pub use outline::OutlineEntry;
pub use save::{SaveOptions, XrefFormat};
pub use text::{decode_text_string, encode_text_string, is_standard_text_string};
