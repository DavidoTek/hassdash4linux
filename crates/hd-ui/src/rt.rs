//! Bridge between the GTK main loop and a background tokio runtime.
//!
//! All network work runs on tokio (the Home Assistant client needs its timers
//! and sockets); GTK code awaits the results with [`on_tokio`] from
//! `glib::spawn_future_local` futures.

use std::future::Future;
use std::sync::OnceLock;

/// The shared tokio runtime (one worker thread is plenty).
pub fn runtime() -> &'static tokio::runtime::Runtime {
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name("hassdash4linux-io")
            .enable_all()
            .build()
            .expect("cannot start the tokio runtime")
    })
}

/// Run a future on tokio and await its result from any executor.
pub async fn on_tokio<F, T>(f: F) -> T
where
    F: Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    match runtime().spawn(f).await {
        Ok(v) => v,
        Err(e) if e.is_panic() => std::panic::resume_unwind(e.into_panic()),
        Err(e) => panic!("background task failed: {e}"),
    }
}
