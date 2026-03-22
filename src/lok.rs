use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

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
