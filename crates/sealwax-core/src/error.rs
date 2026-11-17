use std::ops::RangeInclusive;

/// Everything that can go wrong in sealwax. Messages never contain document content.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The data is not a PDF sealwax can read, even after repair.
    #[error("PDF is malformed and cannot be read")]
    Malformed,

    /// The document is encrypted and the password given (or the empty one) does not open it.
    #[error("PDF is password protected")]
    PasswordRequired,

    /// The document has a number of pages outside the range the caller accepts.
    #[error("PDF has {count} pages, accepted: {} to {}", accepted.start(), accepted.end())]
    PageCountOutOfRange {
        /// Pages found in the document.
        count: usize,
        /// Range the caller accepts.
        accepted: RangeInclusive<usize>,
    },

    /// The page tree declares more pages (`/Count`) than can be read: a damaged page would
    /// otherwise disappear without a word.
    #[error("PDF declares {declared} pages but only {found} can be read")]
    PagesMissing {
        /// Pages found in the document.
        found: usize,
        /// The `/Count` of the root of the page tree.
        declared: i64,
    },

    /// A configured limit was exceeded.
    #[error("limit exceeded: {what}")]
    LimitExceeded {
        /// Which limit.
        what: &'static str,
    },

    /// The deadline of the operation ([`crate::Limits::deadline`]) has passed.
    #[error("time limit exceeded")]
    Timeout,

    /// An argument the caller gave is not one sealwax can use.
    #[error("invalid argument: {what}")]
    InvalidArgument {
        /// Which argument.
        what: &'static str,
    },

    /// The operation is not supported on this document.
    #[error("not supported: {what}")]
    Unsupported {
        /// What is not supported.
        what: &'static str,
    },

    /// A page still holds a visible widget after another engine was to flatten the form: it did
    /// not.
    #[error("page {page} still holds a visible widget: the form was not flattened")]
    NotFlattened {
        /// The page, from 1.
        page: usize,
    },

    /// A save did not read back as it should; nothing is returned rather than a broken file.
    #[error("saved document failed its check: {what}")]
    SaveCheck {
        /// What did not read back.
        what: &'static str,
    },

    /// Reading a file failed.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Result type of sealwax.
pub type Result<T, E = Error> = std::result::Result<T, E>;
