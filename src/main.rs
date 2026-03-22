#[macro_use]
pub mod multiprocessor;

use anyhow::Result;
use libreofficekit::Office;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Clone)]
pub struct LokInput {
    pub input: PathBuf,
    pub output: PathBuf,
    pub format: String,
}

#[derive(Serialize, Deserialize)]
pub struct LokOutput;

fn lok_init() -> Result<Office> {
    let path = Office::find_install_path()
        .ok_or_else(|| anyhow::anyhow!("LibreOffice not found"))?;
    eprintln!("[Worker] LOK path: {}", path.display());
    Office::new(&path).map_err(|e| anyhow::anyhow!("LOK init failed at {}: {e}", path.display()))
}

fn lok_work(office: &Office, input: LokInput) -> Result<LokOutput> {
    use libreofficekit::DocUrl;
    let input_url = DocUrl::from_path(&input.input)?;
    let output_url = DocUrl::from_path(&input.output)?;
    let mut doc = office.document_load(&input_url)?;
    doc.save_as(&output_url, &input.format, None)?;
    Ok(LokOutput)
}

fork_pool!(LOK_POOL, LokInput => LokOutput, {
    init: lok_init,
    work: lok_work,
    concurrency: 1,
});

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "multi_thread")]
    async fn test_convert() {
        let test_dir = PathBuf::from("/tmp/lok-test-convert");
        std::fs::create_dir_all(&test_dir).unwrap();

        let input = test_dir.join("test.txt");
        let output = test_dir.join("test.pdf");
        std::fs::write(&input, "Hello from LOK test").unwrap();

        let result = LOK_POOL.process(LokInput {
            input: input.clone(),
            output: output.clone(),
            format: "pdf".to_string(),
        }).await;

        match &result {
            Ok(_) => {
                let size = std::fs::metadata(&output).map(|m| m.len()).unwrap_or(0);
                println!("SUCCESS: {} bytes", size);
            }
            Err(e) => println!("FAIL: {e:#}"),
        }
        result.unwrap();
    }
}

fn main() {}
