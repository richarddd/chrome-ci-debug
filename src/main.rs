#[macro_use]
pub mod multiprocessor;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

// === LOK Pool ===
#[derive(Serialize, Deserialize, Clone)]
pub struct LokInput { pub input: PathBuf, pub output: PathBuf, pub format: String }
#[derive(Serialize, Deserialize)]
pub struct LokOutput;

fn lok_init() -> Result<libreofficekit::Office> {
    let path = libreofficekit::Office::find_install_path()
        .ok_or_else(|| anyhow::anyhow!("LibreOffice not found"))?;
    eprintln!("[LOK Worker] init at {}", path.display());
    libreofficekit::Office::new(&path).map_err(|e| anyhow::anyhow!("{e}"))
}
fn lok_work(office: &libreofficekit::Office, input: LokInput) -> Result<LokOutput> {
    use libreofficekit::DocUrl;
    let i = DocUrl::from_path(&input.input)?;
    let o = DocUrl::from_path(&input.output)?;
    let mut doc = office.document_load(&i)?;
    doc.save_as(&o, &input.format, None)?;
    Ok(LokOutput)
}
fork_pool!(LOK_POOL, LokInput => LokOutput, { init: lok_init, work: lok_work, concurrency: 1 });

// === Dummy second pool (simulates PDF_POOL) ===
#[derive(Serialize, Deserialize, Clone)]
pub struct DummyIn(u32);
#[derive(Serialize, Deserialize)]
pub struct DummyOut(u32);
fn dummy_init() -> Result<()> { eprintln!("[Dummy Worker] init"); Ok(()) }
fn dummy_work(_: &(), input: DummyIn) -> Result<DummyOut> { Ok(DummyOut(input.0 * 2)) }
fork_pool!(DUMMY_POOL, DummyIn => DummyOut, { init: dummy_init, work: dummy_work, concurrency: 1 });

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "multi_thread")]
    async fn test_dummy_pool() {
        let r = DUMMY_POOL.process(DummyIn(21)).await.unwrap();
        assert_eq!(r.0, 42);
        println!("Dummy pool: OK");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_lok_convert() {
        let dir = PathBuf::from("/tmp/lok-test-ci");
        std::fs::create_dir_all(&dir).unwrap();
        let input = dir.join("test.txt");
        let output = dir.join("test.pdf");
        std::fs::write(&input, "Hello").unwrap();
        LOK_POOL.process(LokInput { input, output: output.clone(), format: "pdf".into() }).await.unwrap();
        assert!(output.exists());
        println!("LOK convert: OK ({} bytes)", std::fs::metadata(&output).unwrap().len());
    }
}

fn main() {}
