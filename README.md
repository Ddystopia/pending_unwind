# `pending_unwind`

Make any future non-unwinding. If [`Future::poll`] panics, it will be converted to [`Poll::Pending`]. One of the biggest problems with linear types is unwinding - what should you do duriing panic? They cannot be dropped, thus you cannot continue unwinding, and abort the program. This crate provides a way to convert unwinding to [`Poll::Pending`], so you can handle it in your own way.

A web server may use this to cancel all futures associated with a thread that panicked without having [`catch_unwind`].

```rust
let fut = pending_unwind(async { panic!() });
let mut fut = Box::pin(fut);
let mut cx = Context::from_waker(Waker::noop());
let poll = fut.as_mut().poll(&mut cx);
assert_eq!(poll, Poll::Pending);
```

