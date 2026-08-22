use std::{
    cell::{Cell, RefCell},
    fmt,
    ops::{Deref, DerefMut},
    rc::Rc,
};

use crate::page::{Page, RendererPageLifetimeOwner};
use crate::{PageId, RendererOwnerLocalHostId};

struct BrowserPageRuntimeState {
    live: Cell<bool>,
    page_id: u64,
    renderer_page_id: PageId,
    renderer_owner_local_host_id: RendererOwnerLocalHostId,
    page: RefCell<Option<Page>>,
}

/// Unique Browser Host authority for one mutable physical `Page` payload.
///
/// The paired access capability may be cloned into protocol projections, but
/// only this owner can keep the payload live. Retiring the owner removes the
/// `Page` from the shared cell before any stale frontend projection can use it.
pub struct BrowserPageRuntimeOwner {
    state: Rc<BrowserPageRuntimeState>,
    owns_live_payload: bool,
}

/// Cloneable, non-owning command/cache access to a Browser-owned `Page`.
///
/// This value deliberately keeps only the allocation alive. Once the unique
/// owner retires, `checkout_page` returns `None`; retaining or dropping a
/// frontend projection therefore cannot extend Browser Page residence. A
/// checkout also returns `None` while another lease temporarily owns the
/// payload. Callers must use `is_live` for residence tests and release a lease
/// before a two-phase command checks out the same Page for its finish turn.
#[derive(Clone)]
pub struct BrowserPageRuntimeAccess {
    state: Rc<BrowserPageRuntimeState>,
}

/// Move-owned access to one exact Browser-owned `Page` payload.
///
/// Checking out the payload deliberately ends the `RefCell` borrow before the
/// caller receives this value, so a renderer command may retain the lease
/// across an async wait without borrowing Browser Host state. On drop the
/// payload returns only while the same runtime owner remains live. A
/// replacement or Target termination invalidates the owner first, causing a
/// late lease to discard the stale Page instead of restoring it into the new
/// generation.
pub struct BrowserPageRuntimeLease {
    state: Rc<BrowserPageRuntimeState>,
    page: Option<Page>,
}

impl BrowserPageRuntimeOwner {
    pub fn new(page: Page) -> Self {
        let page_id = page.page_id();
        let renderer_page_id = page.renderer_page_id();
        let renderer_owner_local_host_id = page.renderer_owner_local_host_id();
        Self {
            state: Rc::new(BrowserPageRuntimeState {
                live: Cell::new(true),
                page_id,
                renderer_page_id,
                renderer_owner_local_host_id,
                page: RefCell::new(Some(page)),
            }),
            owns_live_payload: true,
        }
    }

    pub fn access(&self) -> BrowserPageRuntimeAccess {
        BrowserPageRuntimeAccess {
            state: Rc::clone(&self.state),
        }
    }

    pub fn page_id(&self) -> u64 {
        self.state.page_id
    }

    pub fn take_renderer_lifetime_owner(&mut self) -> Option<RendererPageLifetimeOwner> {
        self.state
            .page
            .borrow_mut()
            .as_mut()
            .and_then(Page::take_renderer_lifetime_owner)
    }

    pub fn try_restore_renderer_lifetime_owner(
        &mut self,
        owner: RendererPageLifetimeOwner,
    ) -> Result<(), RendererPageLifetimeOwner> {
        let mut page = self.state.page.borrow_mut();
        let Some(page) = page.as_mut() else {
            return Err(owner);
        };
        page.try_restore_renderer_lifetime_owner(owner)
    }

    /// Returns the still-owned candidate after a rejected Browser transaction.
    /// A committed owner is moved into the registry and cannot use this path.
    pub fn into_page(mut self) -> Option<Page> {
        self.state.live.set(false);
        let page = self.state.page.borrow_mut().take();
        self.owns_live_payload = false;
        page
    }

    fn retire(&mut self) {
        if !self.owns_live_payload {
            return;
        }
        self.owns_live_payload = false;
        self.state.live.set(false);
        if let Ok(mut page) = self.state.page.try_borrow_mut() {
            let _ = page.take();
        }
    }
}

impl Drop for BrowserPageRuntimeOwner {
    fn drop(&mut self) {
        self.retire();
    }
}

impl fmt::Debug for BrowserPageRuntimeOwner {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BrowserPageRuntimeOwner")
            .field("live", &self.state.live.get())
            .field("page_id", &self.page_id())
            .finish_non_exhaustive()
    }
}

impl BrowserPageRuntimeAccess {
    pub fn is_live(&self) -> bool {
        self.state.live.get()
    }

    pub fn checkout_page(&self) -> Option<BrowserPageRuntimeLease> {
        if !self.state.live.get() {
            return None;
        }
        let page = self.state.page.try_borrow_mut().ok()?.take()?;
        Some(BrowserPageRuntimeLease {
            state: Rc::clone(&self.state),
            page: Some(page),
        })
    }

    pub fn page_id(&self) -> u64 {
        self.state.page_id
    }

    pub fn renderer_page_id(&self) -> PageId {
        self.state.renderer_page_id
    }

    pub fn renderer_owner_local_host_id(&self) -> RendererOwnerLocalHostId {
        self.state.renderer_owner_local_host_id
    }

    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn from_page_for_test_fixture(page: Page) -> Self {
        let page_id = page.page_id();
        let renderer_page_id = page.renderer_page_id();
        let renderer_owner_local_host_id = page.renderer_owner_local_host_id();
        Self {
            state: Rc::new(BrowserPageRuntimeState {
                live: Cell::new(true),
                page_id,
                renderer_page_id,
                renderer_owner_local_host_id,
                page: RefCell::new(Some(page)),
            }),
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn retire_and_take_page_for_test_fixture(self) -> Option<Page> {
        self.state.live.set(false);
        self.state.page.borrow_mut().take()
    }
}

impl Deref for BrowserPageRuntimeLease {
    type Target = Page;

    fn deref(&self) -> &Self::Target {
        self.page
            .as_ref()
            .expect("a live Browser Page runtime lease must contain its payload")
    }
}

impl DerefMut for BrowserPageRuntimeLease {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.page
            .as_mut()
            .expect("a live Browser Page runtime lease must contain its payload")
    }
}

impl Drop for BrowserPageRuntimeLease {
    fn drop(&mut self) {
        let Some(page) = self.page.take() else {
            return;
        };
        if !self.state.live.get() {
            return;
        }
        let Ok(mut resident) = self.state.page.try_borrow_mut() else {
            self.state.live.set(false);
            return;
        };
        if resident.is_some() {
            self.state.live.set(false);
            return;
        }
        *resident = Some(page);
    }
}

impl fmt::Debug for BrowserPageRuntimeLease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BrowserPageRuntimeLease")
            .field("owner_live", &self.state.live.get())
            .field("page_id", &self.page.as_ref().map(Page::page_id))
            .finish_non_exhaustive()
    }
}

impl fmt::Debug for BrowserPageRuntimeAccess {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BrowserPageRuntimeAccess")
            .field("live", &self.state.live.get())
            .field("page_id", &self.state.page_id)
            .finish_non_exhaustive()
    }
}
