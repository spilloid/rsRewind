//! Ignored by default: requires a Windows OCR language pack to be installed. Run explicitly
//! with `cargo test -p rsrewind-ocr -- --ignored`.

use std::error::Error;

use rsrewind_ocr::{OcrEngine, render_text_to_frame};

#[test]
#[ignore = "requires a Windows OCR language pack installed on this machine"]
fn recognizes_rendered_text() -> Result<(), Box<dyn Error>> {
    let frame = render_text_to_frame("Hello rsRewind OCR", 640, 200)?;

    let engine = OcrEngine::new(None)?;
    let output = engine.recognize(&frame)?;

    let lowercase = output.text.to_lowercase();
    assert!(
        lowercase.contains("hello"),
        "recognized text was: {:?}",
        output.text
    );
    assert!(
        lowercase.contains("rsrewind") || lowercase.contains("rewind"),
        "recognized text was: {:?}",
        output.text
    );
    assert!(
        lowercase.contains("ocr"),
        "recognized text was: {:?}",
        output.text
    );
    assert!(!output.blocks.is_empty(), "expected at least one OcrBlock");

    for (i, block) in output.blocks.iter().enumerate() {
        assert_eq!(block.line_index, i as u32);
        assert!(block.confidence.is_none());
        assert!(block.width > 0.0 && block.height > 0.0);
        // Rects must stay within the (unscaled) source frame: 640x200 here, well under
        // MaxImageDimension, so this also proves no accidental downscale/scale-back drift.
        assert!(block.x >= 0.0 && block.x + block.width <= 640.0 + 1.0);
        assert!(block.y >= 0.0 && block.y + block.height <= 200.0 + 1.0);
    }

    Ok(())
}
