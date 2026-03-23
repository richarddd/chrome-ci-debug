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
        // Skip if already initialized or if we're a forked child
        if self.sender.get().is_some() || IS_FORK_CHILD.load(Ordering::SeqCst) {
            let _ = std::fs::OpenOptions::new().create(true).append(true)
                .open("/tmp/forkpool_deferred.log")
                .and_then(|mut f| {
                    use std::io::Write;
                    writeln!(f, "pid={} SKIP init_pool (sender={}, fork_child={})",
                        std::process::id(),
                        self.sender.get().is_some(),
                        IS_FORK_CHILD.load(Ordering::SeqCst))
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

        for _ in 0..concurrency {
            let (parent_sock, child_sock) =
                std::os::unix::net::UnixStream::pair().expect("Failed to create socketpair");

            match unsafe { fork() }.expect("fork failed") {
                ForkResult::Parent { .. } => {
                    drop(child_sock);
                    streams.push(parent_sock.into_raw_fd());
                }
                ForkResult::Child => {
                    IS_FORK_CHILD.store(true, Ordering::SeqCst);
                    // Kill child when parent process exits.
                    // Uses ppid polling instead of PR_SET_PDEATHSIG because the latter
                    // tracks the forking *thread*, not process — if the forking thread
                    // exits (deferred fork pools), children get killed immediately.
                    let ppid = nix::unistd::getppid();
                    std::thread::spawn(move || {
                        loop {
                            std::thread::sleep(std::time::Duration::from_secs(1));
                            if nix::unistd::getppid() != ppid {
                                std::process::exit(0);
                            }
                        }
                    });
                    drop(parent_sock);
                    child_fd_opt = Some(child_sock.into_raw_fd());
                    break;
                }
            }
        }

        let (tx, rx) = mpsc::unbounded_channel();
        let _ = self.sender.set(tx);

        if let Some(fd) = child_fd_opt {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("Failed to build worker runtime");

            rt.block_on(Self::run_worker(fd, init_fn, work_fn));
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

    async fn run_worker<Ctx, InitFn, WorkFn>(fd: i32, init_fn: InitFn, work_fn: WorkFn)
    where
        Ctx: 'static,
        InitFn: Fn() -> Result<Ctx>,
        WorkFn: Fn(&Ctx, I) -> Result<O>,
    {
        let stream = unsafe { std::os::unix::net::UnixStream::from_raw_fd(fd) };
        stream.set_nonblocking(true).unwrap();
        let stream = UnixStream::from_std(stream).unwrap();

        let mut framed = Framed::new(stream, RmpCodec::<Msg<I, O>>::new());

        let ctx = match init_fn() {
            Ok(c) => {
                let _ = std::fs::OpenOptions::new().create(true).append(true)
                    .open("/tmp/forkpool_deferred.log")
                    .and_then(|mut f| {
                        use std::io::Write;
                        writeln!(f, "pid={} worker init OK, sending Ready", std::process::id())
                    });
                c
            }
            Err(e) => {
                let _ = std::fs::OpenOptions::new().create(true).append(true)
                    .open("/tmp/forkpool_deferred.log")
                    .and_then(|mut f| {
                        use std::io::Write;
                        writeln!(f, "pid={} worker init FAILED: {e:#}", std::process::id())
                    });
                eprintln!("[ForkPool Worker {}] Init failed: {e:#}", std::process::id());
                return;
            }
        };

        use futures::SinkExt;
        if framed.send(Msg::Ready).await.is_err() {
            let _ = std::fs::OpenOptions::new().create(true).append(true)
                .open("/tmp/forkpool_deferred.log")
                .and_then(|mut f| {
                    use std::io::Write;
                    writeln!(f, "pid={} Ready send FAILED", std::process::id())
                });
            return;
        }
        let _ = std::fs::OpenOptions::new().create(true).append(true)
            .open("/tmp/forkpool_deferred.log")
            .and_then(|mut f| {
                use std::io::Write;
                writeln!(f, "pid={} Ready sent, waiting for jobs", std::process::id())
            });

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
        // Wait for pool to be initialized (may be initializing on a background thread)
        while self.sender.get().is_none() {
            std::thread::yield_now();
        }
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
    // Deferred: spawns a thread from #[ctor] that forks after .init_array completes.
    // Use for pools whose init_fn does dlopen (e.g. LibreOffice).
    ($name:ident, $in:ty => $out:ty, { init: $init:path, work: $work:expr $(, concurrency: $n:expr)? , deferred: true $(,)? }) => {
        pub static $name: $crate::multiprocessor::StaticForkPool<$in, $out> =
            $crate::multiprocessor::StaticForkPool::new();

        #[ctor::ctor]
        fn __init_fork_pool_ctor() {
            std::thread::spawn(|| {
                #[cfg(target_os = "linux")]
                unsafe { libc::dlopen(std::ptr::null(), libc::RTLD_NOW); }

                let _ = std::fs::OpenOptions::new().create(true).append(true)
                    .open("/tmp/forkpool_deferred.log")
                    .and_then(|mut f| {
                        use std::io::Write;
                        writeln!(f, "pid={} ppid={} dlopen done, calling init_pool",
                            std::process::id(), nix::unistd::getppid())
                    });
                $name.init_pool($crate::fork_pool!(@concurrency $($n)?), $init, $work);
                let _ = std::fs::OpenOptions::new().create(true).append(true)
                    .open("/tmp/forkpool_deferred.log")
                    .and_then(|mut f| {
                        use std::io::Write;
                        writeln!(f, "pid={} init_pool returned", std::process::id())
                    });
            });
        }
    };
    // Immediate: forks directly in #[ctor]. Use for pools with no dlopen (e.g. pdfium).
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

