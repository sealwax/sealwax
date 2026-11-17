//! sealwax: reading, flattening, PDF/A-3 conversion and PAdES signatures of PDF documents.
//!
//! This crate is the public facade; the work happens in the `sealwax-*` crates. The document
//! model is at the root; each family of operations is a module.

pub use sealwax_core::*;

/// Annotations: a signing page's XFDF applied, annotations flattened, documents merged.
pub use sealwax_annots as annots;
/// A new document written from laid-out pages.
pub use sealwax_compose as compose;
/// Conversion to PDF/A-3 (levels b, u and a).
pub use sealwax_pdfa as pdfa;
/// Pages rendered to images: the grayscale PDF a fax sends.
pub use sealwax_raster as raster;
/// PAdES signatures, timestamps and validation data.
pub use sealwax_sign as sign;
/// The text pages draw: glyphs, and what they map to in Unicode.
pub use sealwax_text as text;
