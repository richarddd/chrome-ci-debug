use anyhow::Result;
use serde::{Deserialize, Serialize};

// Simulate a heavy init like pdfium - dlopen a real library
fn heavy_init() -> Result<()> {
    eprintln!("[Heavy Worker] init - loading libm.so");
    // dlopen a real shared library to simulate pdfium's static init
    unsafe {
        let handle = libc::dlopen(
            b"libm.so.6\0".as_ptr() as *const _,
            libc::RTLD_NOW,
        );
        if handle.is_null() {
            return Err(anyhow::anyhow!("dlopen failed"));
        }
    }
    eprintln!("[Heavy Worker] init OK");
    Ok(())
}

#[derive(Serialize, Deserialize, Clone)]
pub struct HeavyIn(pub u32);
#[derive(Serialize, Deserialize)]
pub struct HeavyOut(pub u32);

fn heavy_work(_: &(), input: HeavyIn) -> Result<HeavyOut> {
    Ok(HeavyOut(input.0 * 2))
}

// Use default concurrency (multiple workers) like PDF_POOL does
fork_pool!(HEAVY_POOL, HeavyIn => HeavyOut, { init: heavy_init, work: heavy_work });
