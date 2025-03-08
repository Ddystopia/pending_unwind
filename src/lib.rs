/*!
Make any future non-unwinding. If [`Future::poll`] panics, it will be converted to [`Poll::Pending`].
One of the biggest problems with linear types is unwinding - what should you do duriing panic? They
cannot be dropped, thus you cannot continue unwinding, and abort the program. This crate provides
a way to convert unwinding to [`Poll::Pending`], so you can handle it in your own way.

A web server may use this to cancel all futures associated with a thread that panicked without having
[`catch_unwind`].
*/
use std::{
    cell::Cell,
    panic::{self, AssertUnwindSafe, catch_unwind},
    pin::Pin,
    task::{Context, Poll},
};

thread_local! {
    static ENTERED: Cell<usize> = Cell::new(0);
}

pin_project_lite::pin_project! {
    /// [`Future`] for the [`pending_unwind`] function. This future is fused.
    #[derive(Debug)]
    #[must_use = "futures do nothing unless you `.await` or poll them"]
    pub struct NoUnwind<Fut> {
        #[pin]
        inner: Option<Fut>,
    }
}

/// Information about nesting depth of the poll.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NestingDepth(pub usize);

/// This functions returns the nesting of [`nounwind_poll`] calls in this thread.
/// `NestingDepth(0)` means that no [`pending_unwind`] is currently nested. This is
/// useful to call inside a panic hook, because if there is some [`pending_unwind`]
/// call, future is permanently stuck in [`Poll::Pending`] state.
pub fn nesting_depth() -> NestingDepth {
    NestingDepth(ENTERED.get())
}

/// Convert any panic during a [`Future::poll`] into a [`Poll::Pending`]. Use panic hook to
/// identify that panic happened and cancel the future on your own terms. For example,
/// web server may use `std::thread::current().id()` and [`nesting_depth`] to identify which
/// thread panicked, and then cancel all futures associated with that thread.
pub fn pending_unwind<F: Future>(fut: F) -> NoUnwind<F>
where
    F: panic::UnwindSafe,
{
    NoUnwind { inner: Some(fut) }
}

impl<Fut: Future> Future for NoUnwind<Fut> {
    type Output = Fut::Output;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Fut::Output> {
        let fut = self.as_mut().project().inner.as_pin_mut();
        let closure = AssertUnwindSafe(|| fut.map(|fut| fut.poll(cx)));

        let layer = ENTERED.get();

        ENTERED.set(layer.saturating_add(1));
        let result = catch_unwind(closure);
        ENTERED.set(layer);

        match result {
            Ok(Some(Poll::Ready(output))) => {
                self.project().inner.set(None);
                Poll::Ready(output)
            }
            Ok(Some(Poll::Pending) | None) => Poll::Pending,
            Err(_) => {
                self.project().inner.set(None);
                Poll::Pending
            }
        }
    }
}

#[cfg(test)]
mod test {
    use std::{
        panic::{self, set_hook},
        sync::{
            Mutex,
            atomic::{AtomicBool, Ordering},
        },
        task::{Context, Poll, Waker},
    };

    use crate::pending_unwind;

    #[test]
    fn usual() {
        let fut = pending_unwind(async { 42 });
        let mut fut = Box::pin(fut);
        let mut cx = Context::from_waker(Waker::noop());
        let poll = fut.as_mut().poll(&mut cx);
        assert_eq!(poll, Poll::Ready(42));
    }

    #[test]
    fn panics_no_hook() {
        let fut = pending_unwind(async { panic!() });
        let mut fut = Box::pin(fut);
        let mut cx = Context::from_waker(Waker::noop());
        let poll = fut.as_mut().poll(&mut cx);
        assert_eq!(poll, Poll::Pending);
    }

    #[test]
    fn panic_without_poll() {
        std::thread::spawn(|| {
            static PANICED: AtomicBool = AtomicBool::new(false);

            let (tx, rx) = std::sync::mpsc::channel();
            let old_hook = Mutex::new(Some(std::panic::take_hook()));
            let thread_id = std::thread::current().id();

            set_hook(Box::new(move |info| {
                let mut old_hook = old_hook.lock().unwrap();

                if std::thread::current().id() != thread_id {
                    if let Some(old_hook) = old_hook.as_ref() {
                        old_hook(info);
                    }
                    return;
                }

                assert!(crate::nesting_depth().0 == 0);
                PANICED.store(true, Ordering::Relaxed);

                let old_hook = old_hook.take().unwrap();
                old_hook(info);
                tx.send(old_hook).unwrap();
            }));

            _ = std::panic::catch_unwind(|| panic!());

            panic::set_hook(rx.recv().unwrap());

            assert!(PANICED.load(Ordering::Relaxed));
        });
    }

    #[test]
    fn panics_and_hook() {
        std::thread::spawn(|| {
            static PANICED: AtomicBool = AtomicBool::new(false);

            let (tx, rx) = std::sync::mpsc::channel();
            let old_hook = Mutex::new(Some(std::panic::take_hook()));
            let thread_id = std::thread::current().id();
            set_hook(Box::new(move |info| {
                let mut old_hook = old_hook.lock().unwrap();

                if std::thread::current().id() != thread_id {
                    if let Some(old_hook) = old_hook.as_ref() {
                        old_hook(info);
                    }
                    return;
                }

                assert!(crate::nesting_depth().0 == 1);
                PANICED.store(true, Ordering::Relaxed);

                let old_hook = old_hook.take().unwrap();
                old_hook(info);
                tx.send(old_hook).unwrap();
            }));

            let fut = pending_unwind(async { panic!() });
            let mut fut = Box::pin(fut);
            let mut cx = Context::from_waker(Waker::noop());
            let poll = fut.as_mut().poll(&mut cx);

            panic::set_hook(rx.recv().unwrap());

            assert_eq!(poll, Poll::Pending);
            assert!(PANICED.load(Ordering::Relaxed));
        });
    }
}
