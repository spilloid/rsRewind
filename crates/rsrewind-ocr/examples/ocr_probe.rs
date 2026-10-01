//! Manual smoke test: prints the OCR languages Windows reports as available, renders a known
//! string into a frame, recognizes it, and prints what came back plus timing.
//!
//! Run with: `cargo run -p rsrewind-ocr --example ocr_probe`

use std::error::Error;

use rsrewind_ocr::{OcrEngine, render_text_to_frame};

const PROBE_TEXT: &str = "Hello rsRewind OCR 12345";

fn main() -> Result<(), Box<dyn Error>> {
    let languages = OcrEngine::available_languages();
    println!("available OCR languages ({}):", languages.len());
    for tag in &languages {
        println!("  - {tag}");
    }

    let max_dim = OcrEngine::max_image_dimension()?;
    println!("OcrEngine::MaxImageDimension() = {max_dim}");

    println!("\nrendering probe frame: {PROBE_TEXT:?}");
    let frame = render_text_to_frame(PROBE_TEXT, 640, 200)?;

    let engine = OcrEngine::new(None)?;
    let output = engine.recognize(&frame)?;

    println!(
        "recognized in {} ms, {} line(s):",
        output.elapsed_ms,
        output.blocks.len()
    );
    for block in &output.blocks {
        println!(
            "  [{}] {:?}  rect=({:.1}, {:.1}, {:.1}x{:.1})",
            block.line_index, block.text, block.x, block.y, block.width, block.height
        );
    }
    println!("\njoined text:\n{}", output.text);

    Ok(())
}
