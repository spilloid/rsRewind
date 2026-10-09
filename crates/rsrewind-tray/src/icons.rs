//! The tray artwork: red (recording) and green (not recording) rsRewind icons, embedded at several
//! sizes (`assets/tray/`, made from `assets/icon/`), decoded once.

use crate::model::{self, Base, Status};

/// `(size, red PNG, green PNG)`.
const SIZES: [(u32, &[u8], &[u8]); 5] = [
    (
        22,
        include_bytes!("../../../assets/tray/red-22.png"),
        include_bytes!("../../../assets/tray/green-22.png"),
    ),
    (
        32,
        include_bytes!("../../../assets/tray/red-32.png"),
        include_bytes!("../../../assets/tray/green-32.png"),
    ),
    (
        48,
        include_bytes!("../../../assets/tray/red-48.png"),
        include_bytes!("../../../assets/tray/green-48.png"),
    ),
    (
        64,
        include_bytes!("../../../assets/tray/red-64.png"),
        include_bytes!("../../../assets/tray/green-64.png"),
    ),
    (
        128,
        include_bytes!("../../../assets/tray/red-128.png"),
        include_bytes!("../../../assets/tray/green-128.png"),
    ),
];

/// Decoded RGBA8 artwork per size.
pub struct Icons {
    images: Vec<(u32, Vec<u8>, Vec<u8>)>,
}

impl Icons {
    pub fn load() -> Result<Self, String> {
        let images = SIZES
            .iter()
            .map(|&(size, red, green)| Ok((size, decode(red, size)?, decode(green, size)?)))
            .collect::<Result<_, String>>()?;
        Ok(Self { images })
    }

    /// Every size, composed for this state, as `(size, ARGB32)`.
    pub fn for_status(&self, status: &Status) -> Vec<(u32, Vec<u8>)> {
        let (base, badge) = model::look(status);
        self.images
            .iter()
            .map(|(size, red, green)| {
                let art = match base {
                    Base::Red => red,
                    Base::Green => green,
                };
                (*size, model::compose(art, *size, badge))
            })
            .collect()
    }
}

/// A `size`x`size` PNG as RGBA8.
fn decode(bytes: &[u8], size: u32) -> Result<Vec<u8>, String> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_transformations(
        png::Transformations::normalize_to_color8() | png::Transformations::ALPHA,
    );
    let mut reader = decoder.read_info().map_err(|e| e.to_string())?;
    let mut buf = vec![0; reader.output_buffer_size().ok_or("icon too large")?];
    let info = reader.next_frame(&mut buf).map_err(|e| e.to_string())?;
    if (info.width, info.height) != (size, size) || info.color_type != png::ColorType::Rgba {
        return Err(format!(
            "icon is {}x{} {:?}, expected {size}x{size} RGBA",
            info.width, info.height, info.color_type
        ));
    }
    buf.truncate(info.buffer_size());
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Recorder;

    #[test]
    fn the_embedded_artwork_decodes_at_every_size() -> Result<(), String> {
        let icons = Icons::load()?;
        let recording = Status {
            recorder: Recorder::Recording,
            privacy_unenforced: false,
        };
        let pixmaps = icons.for_status(&recording);
        assert_eq!(
            pixmaps.iter().map(|p| p.0).collect::<Vec<_>>(),
            [22, 32, 48, 64, 128]
        );
        for (size, argb) in &pixmaps {
            assert_eq!(argb.len(), (size * size * 4) as usize);
        }
        // The red artwork is red where it is opaque; the green one green.
        let dominant = |argb: &[u8]| {
            let (mut r, mut g) = (0u64, 0u64);
            for px in argb.as_chunks::<4>().0.iter().filter(|p| p[0] > 200) {
                r += u64::from(px[1]);
                g += u64::from(px[2]);
            }
            (r, g)
        };
        let (r, g) = dominant(&pixmaps[3].1);
        assert!(r > g, "recording art is red");
        let paused = Status {
            recorder: Recorder::Paused { until: None },
            privacy_unenforced: false,
        };
        let (r, g) = dominant(&icons.for_status(&paused)[3].1);
        assert!(g > r, "paused art is green");
        Ok(())
    }
}
