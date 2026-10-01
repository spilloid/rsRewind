//! Typed errors for `rsrewind-ocr`.

/// Result alias for this crate.
pub type Result<T, E = OcrError> = std::result::Result<T, E>;

/// Everything that can go wrong constructing or running the OCR engine.
#[derive(Debug, thiserror::Error)]
pub enum OcrError {
    /// `BgraFrame::is_well_formed()` returned `false` (stride/pixel buffer inconsistent with
    /// the declared width/height).
    #[error("frame is malformed: pixel buffer does not match width/height/stride")]
    MalformedFrame,

    /// Width or height is zero; there is nothing to recognize.
    #[error("frame has zero width or height")]
    EmptyFrame,

    /// The pixel buffer is larger than `u32::MAX` bytes, which `Windows.Storage.Streams.Buffer`
    /// cannot represent. No real screen capture gets anywhere close to this; it exists so the
    /// length conversion before building the `IBuffer` has a typed failure instead of a panic.
    #[error("frame pixel buffer ({byte_len} bytes) is too large for a WinRT buffer")]
    FrameTooLarge { byte_len: usize },

    /// `Windows.Globalization.Language.CreateLanguage` rejected the requested BCP-47 tag.
    #[error("'{tag}' is not a well-formed language tag: {source}")]
    InvalidLanguageTag {
        tag: String,
        #[source]
        source: windows::core::Error,
    },

    /// No OCR engine could be created, either because the requested language has no OCR
    /// language pack installed, or because no profile language has one. This is the single
    /// most common failure on a fresh Windows install and needs an actionable message: Windows
    /// returns a null engine (not a descriptive error) in this case, so we synthesize our own.
    #[error("{message}")]
    NoLanguage {
        requested: Option<String>,
        message: String,
    },

    /// Any other failure returned by a Windows.Media.Ocr / Windows.Graphics.Imaging call.
    #[error("Windows OCR call failed: {0}")]
    Windows(#[from] windows::core::Error),

    /// A GDI call used only by the test/example text renderer ([`crate::gdi_render`]) returned
    /// an invalid handle. Never produced by `recognize`.
    #[error("GDI call failed: {0}")]
    GdiFailure(&'static str),
}

impl OcrError {
    pub(crate) fn no_language(requested: Option<&str>) -> Self {
        let message = match requested {
            Some(tag) => format!(
                "no OCR language pack is installed for '{tag}'. Install one via Settings \
                 -> Time & language -> Language & region -> Add a language (or select the \
                 language and \"Language options\"), then add the \"Optical character \
                 recognition\" optional feature for that language."
            ),
            None => "no OCR language pack is installed for any of your display languages. \
                     Install one via Settings -> Time & language -> Language & region -> Add \
                     a language, then add the \"Optical character recognition\" optional \
                     feature for that language."
                .to_string(),
        };
        Self::NoLanguage {
            requested: requested.map(str::to_owned),
            message,
        }
    }
}
