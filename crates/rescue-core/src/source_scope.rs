//! Per-worker source grants. Recursive sources must authorize every member before opening it.
use crate::{Error, Result};
use std::{
    cell::RefCell,
    collections::BTreeSet,
    marker::PhantomData,
    path::{Path, PathBuf},
    rc::Rc,
};
thread_local! {static ALLOWED:RefCell<Option<BTreeSet<PathBuf>>>=const{RefCell::new(None)};}
pub struct SourceScope {
    previous: Option<BTreeSet<PathBuf>>,
    _thread: PhantomData<Rc<()>>,
}
impl SourceScope {
    pub fn enter(paths: impl IntoIterator<Item = PathBuf>) -> Self {
        let requested = paths.into_iter().collect::<BTreeSet<_>>();
        let previous = ALLOWED.with(|slot| {
            let mut slot = slot.borrow_mut();
            let next = slot
                .as_ref()
                .map(|old| old.intersection(&requested).cloned().collect())
                .unwrap_or(requested);
            slot.replace(next)
        });
        Self {
            previous,
            _thread: PhantomData,
        }
    }
}
impl Drop for SourceScope {
    fn drop(&mut self) {
        ALLOWED.with(|s| *s.borrow_mut() = self.previous.take());
    }
}
pub(crate) fn check(path: &Path) -> Result<()> {
    ALLOWED.with(|s| {
        if s.borrow().as_ref().is_some_and(|set| !set.contains(path)) {
            Err(Error::Request(
                "source member is outside the allowlist".into(),
            ))
        } else {
            Ok(())
        }
    })
}
