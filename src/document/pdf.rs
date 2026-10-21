//! PDF files.

use anyhow::bail;
use std::path::Path;

pub(super) fn read_pdf(path: &Path) -> anyhow::Result<String> {
    let bytes = std::fs::read(path)?;
    // The PDF parser can panic on malformed files; contain that to this call.
    match std::panic::catch_unwind(|| pdf_extract::extract_text_from_mem(&bytes)) {
        Ok(Ok(text)) => Ok(text),
        Ok(Err(e)) => {
            log::warn!("PDF extraction failed: {e}");
            bail!("the PDF could not be read. It may be damaged or password protected.")
        }
        Err(_) => bail!("the PDF could not be read because it is malformed"),
    }
}
