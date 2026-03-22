use crate::fork_pool;
use anyhow::Result;
use pdfium_render::prelude::*;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

static WARMUP: &[u8] = include_bytes!("../fixtures/sample.pdf");

fn load_pdfium() -> Result<Pdfium> {
    eprintln!("[PDF Worker] init");
    let pdfium = Pdfium::new(Pdfium::bind_to_statically_linked_library()?);
    warmup(&pdfium);
    eprintln!("[PDF Worker] init OK");
    Ok(pdfium)
}

fn warmup(pdfium: &Pdfium) {
    if let Ok(doc) = pdfium.load_pdf_from_byte_slice(WARMUP, None) {
        for page in doc.pages().iter() {
            let _ = page.text().map(|t| t.all());
        }
    }
}

#[derive(Serialize, Deserialize, Clone)]
pub struct PdfInput {
    pub bytes: Vec<u8>,
    pub task: PdfTask,
}

#[derive(Serialize, Deserialize, Clone)]
pub enum PdfTask {
    Full { filename: Arc<str> },
    TextOnly,
}

#[derive(Serialize, Deserialize)]
pub struct PdfOutput {
    pub text: String,
}

fn pdf_process(pdfium: &Pdfium, input: PdfInput) -> Result<PdfOutput> {
    let doc = pdfium.load_pdf_from_byte_slice(&input.bytes, None)?;
    let mut text = String::new();
    for page in doc.pages().iter() {
        if let Ok(t) = page.text() {
            text.push_str(&t.all());
        }
    }
    Ok(PdfOutput { text })
}

fork_pool!(PDF_POOL, PdfInput => PdfOutput, {
    init: load_pdfium,
    work: pdf_process,
});
