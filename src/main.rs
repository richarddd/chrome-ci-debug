#[macro_use]
pub mod multiprocessor;
pub mod lok;
pub mod pdfium;

#[cfg(test)]
mod tests {
    use super::lok::*;
    use super::pdfium::*;
    use std::path::PathBuf;
    use std::sync::Arc;

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

    #[tokio::test(flavor = "multi_thread")]
    async fn test_pdf_extract() {
        let pdf_bytes = include_bytes!("../fixtures/sample.pdf");
        let result = PDF_POOL.process(PdfInput {
            bytes: pdf_bytes.to_vec(),
            task: PdfTask::TextOnly,
        }).await.unwrap();
        println!("PDF extract: OK ({} chars)", result.text.len());
    }
}

fn main() {}
