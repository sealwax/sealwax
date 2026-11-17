//! The renderer's cache kept from one render to the next: a page rendered again (at another
//! scale, in parts, after another page) does not parse its fonts or draw its glyphs anew.
//!
//! hayro's cache cannot move between threads, so each thread keeps its own, for the document it
//! rendered last: rendering another document drops it, and so does a render that panicked. It
//! holds the document's parsed form (and so its bytes) until then, even once the document itself
//! is dropped. What it keeps grows with what is drawn, so it is made anew once its renders have
//! drawn [`KEPT_PIXELS`] pixels.

use std::any::Any;
use std::cell::RefCell;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

use hayro::RenderCache;
use hayro::hayro_syntax::Pdf;
use sealwax_core::Document;
use self_cell::self_cell;

type Drawn<'a> = RenderCache<'a>;

self_cell!(
    /// A document, and the renderer's cache of what was drawn from it.
    struct Kept {
        owner: Arc<Pdf>,

        #[not_covariant]
        dependent: Drawn,
    }
);

/// The pixels a kept cache's renders draw before it is made anew: 4 times the default bound of
/// one render.
pub(crate) const KEPT_PIXELS: u64 = 200_000_000;

struct Slot {
    kept: Kept,
    /// The pixels the renders with this cache drew.
    drawn: u64,
}

thread_local! {
    static KEPT: RefCell<Option<Slot>> = const { RefCell::new(None) };
}

/// Run `draw`, an image of `pixels` pixels, on `doc`'s parsed form with the renderer's cache this
/// thread keeps for it: made anew when the thread last rendered another document, or once its
/// renders have drawn [`KEPT_PIXELS`]. A call made while another runs on the same thread gets a
/// cache of its own. A panic of `draw` is returned, and the cache it used dropped.
pub(crate) fn draw<R>(
    doc: &Document,
    pixels: u64,
    draw: impl for<'a> FnOnce(&'a Pdf, &RenderCache<'a>) -> R,
) -> Result<R, Box<dyn Any + Send>> {
    let pdf = doc.hayro_shared();
    KEPT.with(|slot| match slot.try_borrow_mut() {
        Ok(mut slot) => {
            let reusable = slot.as_ref().is_some_and(|slot| {
                Arc::ptr_eq(slot.kept.borrow_owner(), &pdf) && slot.drawn < KEPT_PIXELS
            });
            if !reusable {
                // The cache kept before goes first: two never live at once.
                *slot = None;
                *slot = Some(Slot {
                    kept: Kept::new(pdf, |_| RenderCache::new()),
                    drawn: 0,
                });
            }
            let Some(kept) = slot.as_mut() else {
                return catch_unwind(AssertUnwindSafe(|| draw(doc.hayro(), &RenderCache::new())));
            };
            kept.drawn = kept.drawn.saturating_add(pixels);
            let drawn = catch_unwind(AssertUnwindSafe(|| {
                kept.kept.with_dependent(|owner, cache| draw(owner, cache))
            }));
            if drawn.is_err() {
                // What a panic left in the cache is not to be read again.
                *slot = None;
            }
            drawn
        }
        Err(_) => catch_unwind(AssertUnwindSafe(|| draw(doc.hayro(), &RenderCache::new()))),
    })
}

/// Drop the renderer's cache this thread keeps, and the document it holds.
pub fn release_cache() {
    KEPT.with(|slot| {
        if let Ok(mut slot) = slot.try_borrow_mut() {
            *slot = None;
        }
    });
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic)]

    use std::sync::Arc;

    use hayro::hayro_syntax::Pdf;
    use sealwax_core::{Document, OpenOptions};

    use super::{KEPT, KEPT_PIXELS, draw, release_cache};

    fn document() -> Document {
        let data = b"%PDF-1.7\n1 0 obj<</Type/Catalog/Pages 2 0 R>>endobj 2 0 obj<</Type/Pages/Kids[3 0 R]/Count 1>>endobj 3 0 obj<</Type/Page/Parent 2 0 R/MediaBox[0 0 10 10]>>endobj\ntrailer<</Root 1 0 R>>\n%%EOF";
        Document::open(data.to_vec(), &OpenOptions::default()).expect("opens")
    }

    /// The document the kept cache holds, and the pixels its renders drew.
    fn kept() -> Option<(Arc<Pdf>, u64)> {
        KEPT.with(|slot| {
            slot.borrow()
                .as_ref()
                .map(|slot| (Arc::clone(slot.kept.borrow_owner()), slot.drawn))
        })
    }

    #[test]
    fn a_thread_keeps_the_cache_of_the_document_it_rendered_last() {
        let (one, two) = (document(), document());
        draw(&one, 1, |_, _| ()).expect("draws");
        let (first, _) = kept().expect("kept");
        assert!(Arc::ptr_eq(&first, &one.hayro_shared()));
        // Rendering again keeps it; rendering another document replaces it.
        draw(&one, 1, |_, _| ()).expect("draws");
        let (again, drawn) = kept().expect("kept");
        assert!(Arc::ptr_eq(&again, &first));
        assert_eq!(drawn, 2);
        draw(&two, 1, |_, _| ()).expect("draws");
        assert!(Arc::ptr_eq(&kept().expect("kept").0, &two.hayro_shared()));
        // A call inside another gets a cache of its own, and the outer one is left as it was.
        draw(&two, 1, |_, _| draw(&one, 1, |_, _| ()).expect("draws")).expect("draws");
        assert!(Arc::ptr_eq(&kept().expect("kept").0, &two.hayro_shared()));
        release_cache();
        assert!(kept().is_none());
    }

    #[test]
    fn a_cache_is_made_anew_once_its_renders_drew_enough() {
        let doc = document();
        draw(&doc, KEPT_PIXELS - 1, |_, _| ()).expect("draws");
        // Under the bound before it draws, a render keeps the cache.
        draw(&doc, 1, |_, _| ()).expect("draws");
        assert_eq!(kept().expect("kept").1, KEPT_PIXELS);
        // At the bound, the next one makes another, which counts its pixels alone.
        draw(&doc, 1, |_, _| ()).expect("draws");
        let (owner, drawn) = kept().expect("kept");
        assert!(Arc::ptr_eq(&owner, &doc.hayro_shared()));
        assert_eq!(drawn, 1);
        release_cache();
    }

    #[test]
    fn a_render_that_panicked_drops_the_cache_and_returns_the_panic() {
        let doc = document();
        draw(&doc, 1, |_, _| ()).expect("draws");
        let panicked = draw(&doc, 1, |_, _| -> () { panic!("in the renderer") });
        assert!(panicked.is_err());
        assert!(kept().is_none());
        // The next render makes a cache again.
        draw(&doc, 1, |_, _| ()).expect("draws");
        assert!(kept().is_some());
        release_cache();
    }
}
