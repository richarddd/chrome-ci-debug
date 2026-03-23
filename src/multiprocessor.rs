use anyhow::{Result, anyhow};
use bytes::{Buf, BufMut, BytesMut};
use nix::{
    sys::wait::{WaitPidFlag, WaitStatus, waitpid},
    unistd::{ForkResult, Pid, fork},
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{
    collections::{HashMap, VecDeque},
    os::unix::io::{FromRawFd, IntoRawFd},
    panic::AssertUnwindSafe,
    sync::{Arc, OnceLock, atomic::{AtomicBool, Ordering}},
};

static IS_FORK_CHILD: AtomicBool = AtomicBool::new(false);
use tokio::{
    net::UnixStream,
    sync::{mpsc, oneshot},
};
use tokio_util::codec::{Decoder, Encoder, Framed};
use tracing;

// --- Protocol & Codec ---

#[derive(Serialize, Deserialize)]
enum Msg<I, O> {
    Ready,
    Job {
        id: u64,
        input: I,
    },
    Done {
        id: u64,
        output: std::result::Result<O, String>,
    },
}

/// A Codec that writes a 4-byte Little Endian length, followed by MessagePack data.
struct RmpCodec<T>(std::marker::PhantomData<T>);

impl<T> RmpCodec<T> {
    fn new() -> Self {
        Self(std::marker::PhantomData)
    }
}

impl<T: Serialize> Encoder<T> for RmpCodec<T> {
    type Error = anyhow::Error;

    fn encode(&mut self, item: T, dst: &mut BytesMut) -> Result<()> {
        // Serialize to vector first
        let data = rmp_serde::to_vec(&item).map_err(|e| anyhow!("Serialization error: {}", e))?;
        let len = data.len();

        // Reserve space: 4 bytes for length + body length
        dst.reserve(4 + len);

        // Write Length (Little Endian)
        dst.put_u32_le(len as u32);

        // Write Body
        dst.extend_from_slice(&data);

        Ok(())
    }
}

impl<T: DeserializeOwned> Decoder for RmpCodec<T> {
    type Item = T;
    type Error = anyhow::Error;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>> {
        // We need at least 4 bytes to read the length
        if src.len() < 4 {
            return Ok(None);
        }

        // Read the length without advancing the buffer yet (peek)
        let mut len_bytes = [0u8; 4];
        len_bytes.copy_from_slice(&src[..4]);
        let len = u32::from_le_bytes(len_bytes) as usize;

        // Check if we have the full message
        if src.len() < 4 + len {
            // Reserve needed capacity to avoid multiple small reallocations
            src.reserve(4 + len - src.len());
            return Ok(None);
        }

        // Advance past the length prefix
        src.advance(4);

        // Deserialize the payload
        // rmp_serde::from_read is risky with BytesMut, from_slice is safer here
        let item = rmp_serde::from_slice(&src[..len])
            .map_err(|e| anyhow!("Deserialization error: {}", e))?;

        // Advance past the payload
        src.advance(len);

        Ok(Some(item))
    }
}

// --- Internal Events ---

enum Event<I, O> {
    Msg(usize, Msg<I, O>),
    Died(usize),
}

// --- Pool Implementation ---

pub struct StaticForkPool<I, O> {
    sender: OnceLock<mpsc::UnboundedSender<(I, oneshot::Sender<Result<O>>)>>,
}

impl<I, O> Default for StaticForkPool<I, O>
where
    I: Serialize + DeserializeOwned + Send + Sync + Clone + 'static,
    O: Serialize + DeserializeOwned + Send + Sync + 'static,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<I, O> StaticForkPool<I, O>
where
    I: Serialize + DeserializeOwned + Send + Sync + Clone + 'static,
    O: Serialize + DeserializeOwned + Send + Sync + 'static,
{
    pub const fn new() -> Self {
        Self {
            sender: OnceLock::new(),
        }
    }

    pub fn init_pool<Ctx, InitFn, WorkFn>(
        &self,
        concurrency: usize,
        init_fn: InitFn,
        work_fn: WorkFn,
    ) where
        Ctx: 'static,
        InitFn: Fn() -> Result<Ctx> + Clone + Send + 'static,
        WorkFn: Fn(&Ctx, I) -> Result<O> + Copy + Send + 'static,
    {
        self.init_pool_inner(concurrency, init_fn, None, work_fn);
    }

    /// Like init_pool but calls init_fn in the parent before forking.
    /// The child inherits the context via fork's memory copy and skips init_fn.
    /// Use this when init_fn does dlopen which deadlocks after fork during .init_array.
    pub fn init_pool_prefork<Ctx, InitFn, WorkFn>(
        &self,
        concurrency: usize,
        init_fn: InitFn,
        work_fn: WorkFn,
    ) where
        Ctx: 'static,
        InitFn: Fn() -> Result<Ctx> + Clone + Send + 'static,
        WorkFn: Fn(&Ctx, I) -> Result<O> + Copy + Send + 'static,
    {
        match init_fn() {
            Ok(ctx) => self.init_pool_inner(concurrency, init_fn, Some(ctx), work_fn),
            Err(e) => {
                eprintln!("[ForkPool] prefork init failed: {e:#}");
                // Still set up the pool so process() returns errors instead of panicking
                let (tx, _rx) = mpsc::unbounded_channel();
                let _ = self.sender.set(tx);
            }
        }
    }

    fn init_pool_inner<Ctx, InitFn, WorkFn>(
        &self,
        concurrency: usize,
        init_fn: InitFn,
        prefork_ctx: Option<Ctx>,
        work_fn: WorkFn,
    ) where
        Ctx: 'static,
        InitFn: Fn() -> Result<Ctx> + Clone + Send + 'static,
        WorkFn: Fn(&Ctx, I) -> Result<O> + Copy + Send + 'static,
    {
        // Skip if we're already a forked child from another pool
        if IS_FORK_CHILD.load(Ordering::SeqCst) {
            let _ = std::fs::OpenOptions::new()
                .create(true).append(true)
                .open("/tmp/forkpool.log")
                .and_then(|mut f| {
                    use std::io::Write;
                    writeln!(f, "pid={} SKIPPED-init_pool (is_fork_child=true)", std::process::id())
                });
            return;
        }

        let concurrency = if concurrency == 0 {
            std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(4)
        } else {
            concurrency
        };

        let mut streams = Vec::with_capacity(concurrency);
        let mut child_fd_opt: Option<i32> = None;

        for i in 0..concurrency {
            let (parent_sock, child_sock) =
                std::os::unix::net::UnixStream::pair().expect("Failed to create socketpair");

            let _ = std::fs::OpenOptions::new()
                .create(true).append(true)
                .open("/tmp/forkpool.log")
                .and_then(|mut f| {
                    use std::io::Write;
                    writeln!(f, "pid={} FORKING worker {i}/{concurrency}", std::process::id())
                });

            match unsafe { fork() }.expect("fork failed") {
                ForkResult::Parent { .. } => {
                    drop(child_sock);
                    streams.push(parent_sock.into_raw_fd());
                }
                ForkResult::Child => {
                    IS_FORK_CHILD.store(true, Ordering::SeqCst);
                    // Die when parent exits
                    #[cfg(target_os = "linux")]
                    unsafe {
                        libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
                    }
                    #[cfg(target_os = "macos")]
                    {
                        let ppid = nix::unistd::getppid().as_raw();
                        std::thread::spawn(move || unsafe {
                            let kq = libc::kqueue();
                            let mut event: libc::kevent = std::mem::zeroed();
                            event.ident = ppid as usize;
                            event.filter = libc::EVFILT_PROC;
                            event.flags = libc::EV_ADD;
                            event.fflags = libc::NOTE_EXIT;
                            libc::kevent(kq, &event, 1, &mut event, 1, std::ptr::null());
                            std::process::exit(1);
                        });
                    }
                    drop(parent_sock);
                    child_fd_opt = Some(child_sock.into_raw_fd());
                    break;
                }
            }
        }

        let (tx, rx) = mpsc::unbounded_channel();
        if self.sender.set(tx).is_err() {
            panic!("StaticForkPool initialized twice");
        }

        if let Some(fd) = child_fd_opt {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("Failed to build worker runtime");

            rt.block_on(Self::run_worker(fd, prefork_ctx, init_fn, work_fn));
            std::process::exit(0);
        }

        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("Failed to build router runtime");
            rt.block_on(Self::run_router(streams, rx));
        });
    }

    async fn run_worker<Ctx, InitFn, WorkFn>(fd: i32, prefork_ctx: Option<Ctx>, init_fn: InitFn, work_fn: WorkFn)
    where
        Ctx: 'static,
        InitFn: Fn() -> Result<Ctx>,
        WorkFn: Fn(&Ctx, I) -> Result<O>,
    {
        let pid = std::process::id();
        let log = |step: &str| {
            let _ = std::fs::OpenOptions::new()
                .create(true).append(true)
                .open("/tmp/forkpool.log")
                .and_then(|mut f| {
                    use std::io::Write;
                    writeln!(f, "pid={pid} {step}")
                });
        };
        log("1-worker-start");
        let stream = unsafe { std::os::unix::net::UnixStream::from_raw_fd(fd) };
        log("2-from-raw-fd");
        stream.set_nonblocking(true).unwrap();
        log("3-set-nonblocking");
        let stream = UnixStream::from_std(stream).unwrap();
        log("4-unix-stream");

        let mut framed = Framed::new(stream, RmpCodec::<Msg<I, O>>::new());
        log("5-framed");

        let ctx = if let Some(ctx) = prefork_ctx {
            log("6-using-prefork-ctx");
            ctx
        } else {
            match init_fn() {
                Ok(c) => { log("6-init-ok"); c }
                Err(e) => {
                    log(&format!("6-init-FAILED: {e:#}"));
                    return;
                }
            }
        };

        use futures::SinkExt;
        log("7-sending-ready");
        if framed.send(Msg::Ready).await.is_err() {
            log("7-send-ready-FAILED");
            return;
        }
        log("8-ready-sent");

        use futures::StreamExt;
        while let Some(Ok(msg)) = framed.next().await {
            if let Msg::Job { id, input } = msg {
                // Catch panics in work_fn to prevent silent worker death
                let result = std::panic::catch_unwind(AssertUnwindSafe(|| work_fn(&ctx, input)));

                let output = match result {
                    Ok(Ok(o)) => Ok(o),
                    Ok(Err(e)) => Err(e.to_string()),
                    Err(p) => {
                        let msg = if let Some(s) = p.downcast_ref::<&str>() {
                            format!("Panic: {}", s)
                        } else {
                            "Panic: (unknown)".to_string()
                        };
                        Err(msg)
                    }
                };

                if framed.send(Msg::Done { id, output }).await.is_err() {
                    break;
                }
            }
        }
    }

    async fn run_router(
        fds: Vec<i32>,
        mut job_rx: mpsc::UnboundedReceiver<(I, oneshot::Sender<Result<O>>)>,
    ) {
        use futures::{SinkExt, StreamExt};

        let (event_tx, mut event_rx) = mpsc::channel::<Event<I, O>>(fds.len() * 2);
        let mut cmd_txs = Vec::with_capacity(fds.len());

        for (id, fd) in fds.into_iter().enumerate() {
            let stream = unsafe { std::os::unix::net::UnixStream::from_raw_fd(fd) };
            stream.set_nonblocking(true).ok();
            let stream = UnixStream::from_std(stream).unwrap();
            let (r, w) = stream.into_split();

            let (cmd_tx, mut cmd_rx) = mpsc::channel::<Msg<I, O>>(8);
            cmd_txs.push(cmd_tx);

            // Writer Task
            tokio::spawn(async move {
                let mut framed_write = Framed::new(w, RmpCodec::new());
                while let Some(msg) = cmd_rx.recv().await {
                    if framed_write.send(msg).await.is_err() {
                        break;
                    }
                }
            });

            // Reader Task
            let event_tx = event_tx.clone();
            tokio::spawn(async move {
                let mut framed_read = Framed::new(r, RmpCodec::<Msg<I, O>>::new());
                while let Some(res) = framed_read.next().await {
                    match res {
                        Ok(msg) => {
                            if event_tx.send(Event::Msg(id, msg)).await.is_err() {
                                break;
                            }
                        }
                        Err(_) => break, // Decode error or disconnect
                    }
                }
                let _ = event_tx.send(Event::Died(id)).await;
            });
        }

        let mut queue: VecDeque<(u64, Arc<I>)> = VecDeque::new();
        let mut pending: HashMap<u64, oneshot::Sender<Result<O>>> = HashMap::new();
        let mut in_flight: HashMap<usize, (u64, Arc<I>)> = HashMap::new();
        let mut idle: Vec<usize> = Vec::with_capacity(cmd_txs.len());
        let mut dead_workers: usize = 0;
        let mut ctr: u64 = 0;

        let mut sig_child = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::child())
            .expect("Failed to bind SIGCHLD");

        loop {
            // Assign work to idle workers
            while let Some(&wid) = idle.last() {
                if queue.is_empty() {
                    break;
                }
                let (id, input) = queue.pop_front().unwrap();

                // Clone the Arc to keep input in memory (for retries), then clone underlying data for Msg
                let msg = Msg::Job {
                    id,
                    input: (*input).clone(),
                };

                if cmd_txs[wid].try_send(msg).is_ok() {
                    in_flight.insert(wid, (id, input));
                    idle.pop();
                } else {
                    queue.push_front((id, input));
                    break;
                }
            }

            // Check if system is dead
            if dead_workers == cmd_txs.len() && !pending.is_empty() {
                for (_, tx) in pending.drain() {
                    let _ = tx.send(Err(anyhow!("All {dead_workers} workers died — likely init failure (e.g. LibreOffice SDK not found)")));
                }
                break;
            }

            tokio::select! {
                // 1. New Job
                Some((input, tx)) = job_rx.recv() => {
                    ctr = ctr.wrapping_add(1);
                    pending.insert(ctr, tx);
                    queue.push_back((ctr, Arc::new(input)));
                }

                // 2. Internal Events
                Some(event) = event_rx.recv() => {
                    match event {
                        Event::Msg(wid, Msg::Ready) => {
                            idle.push(wid);
                        }
                        Event::Msg(wid, Msg::Done { id, output }) => {
                            in_flight.remove(&wid);
                            if let Some(tx) = pending.remove(&id) {
                                let _ = tx.send(output.map_err(|s| anyhow!(s)));
                            }
                            idle.push(wid);
                        }
                        Event::Died(wid) => {
                            tracing::error!("Worker {wid} died — check earlier logs for init/runtime errors");
                            dead_workers += 1;
                            // Re-queue the job if the worker died processing it
                            if let Some((id, input)) = in_flight.remove(&wid) {
                                queue.push_front((id, input));
                            }
                        }
                        Event::Msg(_, Msg::Job { .. }) => {
                            // Router should never receive Job messages; ignore
                        }
                    }
                }

                // 3. Reap Zombies
                _ = sig_child.recv() => {
                     while let Ok(WaitStatus::Exited(..) | WaitStatus::Signaled(..)) =
                        waitpid(Pid::from_raw(-1), Some(WaitPidFlag::WNOHANG)) {}
                }
            }
        }
    }

    pub fn process(&self, input: I) -> impl std::future::Future<Output = Result<O>> + Send {
        let pool_result = (|| {
            let tx = self
                .sender
                .get()
                .ok_or_else(|| anyhow!("Pool not initialized"))?;
            let (resp_tx, resp_rx) = oneshot::channel();
            tx.send((input, resp_tx))
                .map_err(|_| anyhow!("Pool router closed"))?;
            Ok(resp_rx)
        })();

        async move {
            match pool_result {
                Ok(rx) => rx.await.map_err(|_| anyhow!("Job cancelled"))?,
                Err(e) => Err(e),
            }
        }
    }
}

#[macro_export]
macro_rules! fork_pool {
    ($name:ident, $in:ty => $out:ty, { init: $init:path, work: $work:expr, concurrency: $n:expr, prefork_init: true $(,)? }) => {
        pub static $name: $crate::multiprocessor::StaticForkPool<$in, $out> =
            $crate::multiprocessor::StaticForkPool::new();

        #[ctor::ctor]
        fn __init_fork_pool_ctor() {
            $name.init_pool_prefork($n, $init, $work);
        }
    };
    ($name:ident, $in:ty => $out:ty, { init: $init:path, work: $work:expr $(, concurrency: $n:expr)? $(,)? }) => {
        pub static $name: $crate::multiprocessor::StaticForkPool<$in, $out> =
            $crate::multiprocessor::StaticForkPool::new();

        #[ctor::ctor]
        fn __init_fork_pool_ctor() {
            $name.init_pool($crate::fork_pool!(@concurrency $($n)?), $init, $work);
        }
    };
    (@concurrency) => { 0 };
    (@concurrency $n:expr) => { $n };
}

