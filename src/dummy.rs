use anyhow::Result;
use serde::{Deserialize, Serialize};
use pdfium_render::prelude::*;

#[derive(Serialize, Deserialize, Clone)]
pub struct PdfIn(pub Vec<u8>);
#[derive(Serialize, Deserialize)]
pub struct PdfOut(pub String);

fn pdf_init() -> Result<Pdfium> {
    eprintln!("[PDF Worker] init");
    let pdfium = Pdfium::new(Pdfium::bind_to_statically_linked_library()?);
    eprintln!("[PDF Worker] init OK");
    Ok(pdfium)
}
fn pdf_work(pdfium: &Pdfium, input: PdfIn) -> Result<PdfOut> {
    let doc = pdfium.load_pdf_from_byte_slice(&input.0, None)?;
    let mut text = String::new();
    for page in doc.pages().iter() {
        if let Ok(t) = page.text() {
            text.push_str(&t.all());
        }
    }
    Ok(PdfOut(text))
}
fork_pool!(PDF_POOL, PdfIn => PdfOut, { init: pdf_init, work: pdf_work });
