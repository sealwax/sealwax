//! The interpreter's cache kept from one call to the next: a page's fonts are parsed once per
//! document, not once per call. A document with large embedded font programs on every page spent
//! most of each call parsing them again.
//!
//! hayro's cache cannot move between threads, so each thread keeps its own, for the document it
//! read last: reading another document drops it. It holds the document's parsed form (and so its
//! bytes) until then, even once the document itself is dropped.

use std::cell::RefCell;
use std::sync::Arc;

use hayro_interpret::InterpreterCache;
use hayro_interpret::hayro_syntax::Pdf;
use sealwax_core::Document;
use self_cell::self_cell;

type Fonts<'a> = InterpreterCache<'a>;

self_cell!(
    /// A document, and the cache of what was parsed from it.
    struct Kept {
        owner: Arc<Pdf>,

        #[not_covariant]
        dependent: Fonts,
    }
);

thread_local! {
    static KEPT: RefCell<Option<Kept>> = const { RefCell::new(None) };
}

/// Run `read` on `doc`'s parsed form with the interpreter cache this thread keeps for it, made
/// anew when the thread last read another document. A call made while another runs on the same
/// thread gets a cache of its own.
pub(crate) fn with_cache<R>(
    doc: &Document,
    read: impl for<'a> FnOnce(&'a Pdf, &InterpreterCache<'a>) -> R,
) -> R {
    let pdf = doc.hayro_shared();
    KEPT.with(|kept| match kept.try_borrow_mut() {
        Ok(mut kept) => {
            let same = kept
                .as_ref()
                .is_some_and(|kept| Arc::ptr_eq(kept.borrow_owner(), &pdf));
            if !same {
                // The cache of the document read before goes first: two never live at once.
                *kept = None;
                *kept = Some(Kept::new(pdf, |_| InterpreterCache::new()));
            }
            match kept.as_ref() {
                Some(kept) => kept.with_dependent(|owner, cache| read(owner, cache)),
                None => read(doc.hayro(), &InterpreterCache::new()),
            }
        }
        Err(_) => read(doc.hayro(), &InterpreterCache::new()),
    })
}

/// Drop the cache this thread keeps, and the document it holds.
pub fn release_cache() {
    KEPT.with(|kept| {
        if let Ok(mut kept) = kept.try_borrow_mut() {
            *kept = None;
        }
    });
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use sealwax_core::{Document, OpenOptions};

    use super::{KEPT, release_cache, with_cache};

    fn document() -> Document {
        let data = b"%PDF-1.7\n1 0 obj<</Type/Catalog/Pages 2 0 R>>endobj 2 0 obj<</Type/Pages/Kids[3 0 R]/Count 1>>endobj 3 0 obj<</Type/Page/Parent 2 0 R/MediaBox[0 0 10 10]>>endobj\ntrailer<</Root 1 0 R>>\n%%EOF";
        Document::open(data.to_vec(), &OpenOptions::default()).expect("opens")
    }

    #[test]
    fn a_thread_keeps_the_cache_of_the_document_it_read_last() {
        let (one, two) = (document(), document());
        let kept = || KEPT.with(|kept| kept.borrow().as_ref().map(|k| k.borrow_owner().clone()));
        with_cache(&one, |_, _| ());
        let first = kept().expect("kept");
        assert!(std::sync::Arc::ptr_eq(&first, &one.hayro_shared()));
        // Reading again keeps it; reading another document replaces it.
        with_cache(&one, |_, _| ());
        assert!(std::sync::Arc::ptr_eq(&kept().expect("kept"), &first));
        with_cache(&two, |_, _| ());
        assert!(std::sync::Arc::ptr_eq(
            &kept().expect("kept"),
            &two.hayro_shared()
        ));
        // A call inside another gets a cache of its own, and the outer one is left as it was.
        with_cache(&two, |_, _| with_cache(&one, |_, _| ()));
        assert!(std::sync::Arc::ptr_eq(
            &kept().expect("kept"),
            &two.hayro_shared()
        ));
        release_cache();
        assert!(kept().is_none());
    }
}
