use anyhow::Result;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone)]
pub struct DummyIn(pub u32);
#[derive(Serialize, Deserialize)]
pub struct DummyOut(pub u32);
fn dummy_init() -> Result<()> { eprintln!("[Dummy Worker] init"); Ok(()) }
fn dummy_work(_: &(), input: DummyIn) -> Result<DummyOut> { Ok(DummyOut(input.0 * 2)) }
fork_pool!(DUMMY_POOL, DummyIn => DummyOut, { init: dummy_init, work: dummy_work, concurrency: 1 });
