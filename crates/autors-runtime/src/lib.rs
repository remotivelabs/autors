//! Minimal async runtime abstraction shared by the autors I/O crates.
//! Protocol code (ISO-TP, diagnostics, CCP/XCP, bus devices) is written once
//! in async form against the primitives here, without binding to a concrete
//! executor:
//! - [`sleep`], [`timeout`] and [`spawn_blocking`] run on any executor.
//! - With the default `runtime-tokio` feature they are backed by tokio.
//!   Without it they fall back to a plain [`std::thread`] implementation, so
//!   dependent crates still compile and run with `default-features = false`
//!   (at a higher per-call cost: one thread per pending operation).
//! - [`block_on`] drives a future to completion on the current thread. The
//!   `blocking` feature controls synchronous facade modules in dependent crates;
//!   the executor itself also remains available for explicit worker-thread
//!   bridges used by async transports.

use std::fmt;
use std::future::Future;
use std::task::Poll;
use std::time::Duration;

#[cfg(not(feature = "runtime-tokio"))]
use std::sync::{Arc, Mutex};
#[cfg(not(feature = "runtime-tokio"))]
use std::task::{Context, Waker};

/// Suspends the current task for `duration`.
pub async fn sleep(duration: Duration) {
    imp::sleep(duration).await
}

/// Runs the blocking closure `f` off the async executor and awaits its result.
/// Use this to call synchronous legacy code (vendor driver libraries, serial
/// port reads with timeouts) from async protocol code without stalling the
/// executor.
pub fn spawn_blocking<F, R>(f: F) -> impl Future<Output = R> + Send + 'static
where
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static,
{
    imp::spawn_blocking(f)
}

/// Error returned by [`timeout`] when the deadline elapses before the future
/// completes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Elapsed;

impl fmt::Display for Elapsed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("operation timed out")
    }
}

impl std::error::Error for Elapsed {}

/// Races `future` against [`sleep`]: returns `Ok(output)` if the future
/// completes first, `Err(Elapsed)` when the deadline expires (the future is
/// dropped).
/// Implemented by polling both futures, so it works on any executor.
pub async fn timeout<F: Future>(duration: Duration, future: F) -> Result<F::Output, Elapsed> {
    let mut future = std::pin::pin!(future);
    let mut deadline = std::pin::pin!(sleep(duration));
    std::future::poll_fn(move |cx| {
        if let Poll::Ready(out) = future.as_mut().poll(cx) {
            return Poll::Ready(Ok(out));
        }
        if deadline.as_mut().poll(cx).is_ready() {
            return Poll::Ready(Err(Elapsed));
        }
        Poll::Pending
    })
    .await
}

/// Drives `future` to completion on the current thread, blocking until it
/// resolves.
/// This powers the synchronous facade modules of the I/O crates. With
/// `runtime-tokio` a shared multi-thread runtime is used (so nested
/// [`sleep`]/[`spawn_blocking`] calls use real timers and a blocking pool);
/// without it a simple park/unpark loop drives the future.
/// # Panics
/// Panics when called from inside an async context of the shared runtime —
/// the same restriction as `tokio::runtime::Runtime::block_on`. Do not call
/// the blocking facade from within async code.
pub fn block_on<F: Future>(future: F) -> F::Output {
    imp::block_on(future)
}

// ---------------------------------------------------------------------------
// Shared state cell backing the std-fallback futures (no tokio).
// ---------------------------------------------------------------------------

/// Result + waker under one lock: storing the result and taking the waker is
/// atomic, so a producer completing between the consumer's result check and
/// waker store cannot lose the wake-up.
#[cfg(not(feature = "runtime-tokio"))]
struct Cell<R> {
    result: Option<R>,
    waker: Option<Waker>,
}

#[cfg(not(feature = "runtime-tokio"))]
struct Shared<R>(Mutex<Cell<R>>);

#[cfg(not(feature = "runtime-tokio"))]
impl<R> Shared<R> {
    fn new() -> Arc<Self> {
        Arc::new(Self(Mutex::new(Cell {
            result: None,
            waker: None,
        })))
    }

    fn complete(&self, value: R) {
        let waker = {
            let mut guard = self.0.lock().unwrap_or_else(|p| p.into_inner());
            guard.result = Some(value);
            guard.waker.take()
        };
        if let Some(waker) = waker {
            waker.wake();
        }
    }

    fn poll_take(&self, cx: &mut Context<'_>) -> Poll<R> {
        let mut guard = self.0.lock().unwrap_or_else(|p| p.into_inner());
        match guard.result.take() {
            Some(value) => Poll::Ready(value),
            None => {
                guard.waker = Some(cx.waker().clone());
                Poll::Pending
            }
        }
    }
}

/// Future that resolves once a std thread has run `f` (or slept, for the
/// fallback timer) and posted the value into the shared cell.
#[cfg(not(feature = "runtime-tokio"))]
struct ThreadFuture<R>
where
    R: Send + 'static,
{
    shared: Arc<Shared<R>>,
    start: Option<Box<dyn FnOnce(Arc<Shared<R>>) + Send>>,
}

#[cfg(not(feature = "runtime-tokio"))]
impl<R> Future for ThreadFuture<R>
where
    R: Send + 'static,
{
    type Output = R;

    fn poll(self: std::pin::Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<R> {
        let this = self.get_mut();
        if let Some(start) = this.start.take() {
            start(Arc::clone(&this.shared));
        }
        this.shared.poll_take(cx)
    }
}

#[cfg(feature = "runtime-tokio")]
mod imp {
    use std::future::Future;
    use std::time::Duration;

    pub(super) async fn sleep(duration: Duration) {
        tokio::time::sleep(duration).await
    }

    pub(super) fn spawn_blocking<F, R>(f: F) -> impl Future<Output = R> + Send + 'static
    where
        F: FnOnce() -> R + Send + 'static,
        R: Send + 'static,
    {
        let handle = tokio::task::spawn_blocking(f);
        async move { handle.await.expect("blocking task panicked") }
    }

    pub(super) fn block_on<F: Future>(future: F) -> F::Output {
        use std::sync::OnceLock;
        static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
        let runtime = RUNTIME.get_or_init(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .expect("failed to build the shared tokio runtime")
        });
        runtime.block_on(future)
    }
}

#[cfg(not(feature = "runtime-tokio"))]
mod imp {
    use super::*;

    pub(super) fn sleep(duration: Duration) -> impl Future<Output = ()> + Send + 'static {
        spawn_blocking(move || std::thread::sleep(duration))
    }

    pub(super) fn spawn_blocking<F, R>(f: F) -> impl Future<Output = R> + Send + 'static
    where
        F: FnOnce() -> R + Send + 'static,
        R: Send + 'static,
    {
        let mut f = Some(f);
        ThreadFuture {
            shared: Shared::new(),
            start: Some(Box::new(move |shared| {
                let f = f.take().expect("started twice");
                std::thread::spawn(move || shared.complete(f()));
            })),
        }
    }

    pub(super) fn block_on<F: Future>(future: F) -> F::Output {
        use std::task::Wake;

        struct ThreadWaker(std::thread::Thread);

        impl Wake for ThreadWaker {
            fn wake(self: Arc<Self>) {
                self.0.unpark();
            }
            fn wake_by_ref(self: &Arc<Self>) {
                self.0.unpark();
            }
        }

        let waker = Waker::from(Arc::new(ThreadWaker(std::thread::current())));
        let mut cx = Context::from_waker(&waker);
        let mut future = std::pin::pin!(future);
        loop {
            match future.as_mut().poll(&mut cx) {
                Poll::Ready(out) => return out,
                Poll::Pending => std::thread::park(),
            }
        }
    }
}

#[cfg(all(test, feature = "blocking"))]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn block_on_drives_plain_future() {
        assert_eq!(block_on(async { 21 * 2 }), 42);
    }

    #[test]
    fn block_on_drives_sleep_and_spawn_blocking() {
        let start = Instant::now();
        let value = block_on(async {
            sleep(Duration::from_millis(10)).await;
            spawn_blocking(|| 7).await
        });
        assert_eq!(value, 7);
        assert!(start.elapsed() >= Duration::from_millis(10));
    }

    #[test]
    fn timeout_returns_output_when_future_wins() {
        let out = block_on(async { timeout(Duration::from_secs(5), async { "fast" }).await });
        assert_eq!(out, Ok("fast"));
    }

    #[test]
    fn timeout_reports_elapsed_when_deadline_wins() {
        let start = Instant::now();
        let out: Result<(), Elapsed> = block_on(async {
            timeout(Duration::from_millis(20), sleep(Duration::from_secs(5))).await
        });
        assert_eq!(out, Err(Elapsed));
        assert!(start.elapsed() < Duration::from_secs(2));
    }

    #[cfg(feature = "runtime-tokio")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn primitives_run_on_tokio() {
        sleep(Duration::from_millis(1)).await;
        assert_eq!(spawn_blocking(|| 3).await, 3);
        let out = timeout(Duration::from_secs(5), async { 9 }).await;
        assert_eq!(out, Ok(9));
        let timed_out: Result<(), Elapsed> =
            timeout(Duration::from_millis(10), sleep(Duration::from_secs(5))).await;
        assert_eq!(timed_out, Err(Elapsed));
    }
}
