//! unity-texture-codec: the one-time converter that reads Unity's crunched textures (R2.13).
//!
//! A crunched texture stores a CRN stream; `crn2blocks` — this kit's own glue, built by build.sh
//! against Unity's open-source crunch fork — turns it back into plain texture blocks the existing
//! DXT decoders read. The kit's files are written into the engine store and built once on demand;
//! the upstream source is cloned at build time and never ships with Craft Anything.

use crate::ToolPathLookup;
use craft_anything_evidence::{ErrorCode, ToolError};
use craft_anything_platform::process::Process;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const ID: &str = "unitycodec";

/// The kit's own files, written into the engine store before the one-time build.
const KIT: &[(&str, &str)] = &[
    ("build.sh", include_str!("unity_codec_kit/build.sh")),
    ("crn2blocks.cpp", include_str!("unity_codec_kit/crn2blocks.cpp")),
    ("kit-shim/malloc.h", include_str!("unity_codec_kit/malloc.h")),
    ("kit-shim/process.h", include_str!("unity_codec_kit/process.h")),
];

/// Where the kit and its build live: the engine store's own folder for this engine.
pub fn kit_dir(store: &Path) -> PathBuf {
    store.join(ID)
}

/// The codec binary: found where the table looks. When it isn't built yet, the kit's files are
/// written into the engine store and the error names the one command that builds it (git,
/// python3 and a C++ compiler, once) — building is a deliberate step a person or the builder
/// takes, never something a check starts on its own.
pub fn ensure(overrides: &dyn ToolPathLookup, store: &Path) -> Result<PathBuf, ToolError> {
    if let Some(p) = crate::locate(ID, overrides, store) {
        return Ok(p);
    }
    let dir = kit_dir(store);
    std::fs::create_dir_all(&dir).map_err(|e| ToolError::io(dir.display(), e))?;
    for (name, text) in KIT {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| ToolError::io(parent.display(), e))?;
        }
        if !path.exists() {
            std::fs::write(&path, text).map_err(|e| ToolError::io(path.display(), e))?;
        }
    }
    if cfg!(windows) {
        return Err(ToolError::unsupported(
            "Unity's crunched textures need the unity-texture-codec converter, which builds with a C++ compiler on macOS or Linux.",
        )
        .with_remediation("Extract crunched textures on a Mac or Linux machine instead."));
    }
    let script = dir.join("build.sh");
    Err(ToolError::new(ErrorCode::EngineMissing, "unity-texture-codec (crn2blocks) isn't built yet.")
        .with_remediation(format!("Run /bin/sh {} once (needs git, python3 and a C++ compiler), then try again.", script.display())))
}

/// One crunched texture decompressed to plain texture blocks.
pub struct CrnBlocks {
    pub width: u32,
    pub height: u32,
    /// The DXT format the blocks are in (0 = DXT1, 2 = DXT5).
    pub dxt_format: u32,
    pub blocks: Vec<u8>,
}

/// Decompress a CRN stream through the converter; `work` is a scratch folder the run may use.
pub fn crn_blocks(bin: &Path, data: &[u8], work: &Path) -> Result<CrnBlocks, ToolError> {
    std::fs::create_dir_all(work).map_err(|e| ToolError::io(work.display(), e))?;
    let input = work.join("in.crn");
    let output = work.join("out.bin");
    std::fs::write(&input, data).map_err(|e| ToolError::io(input.display(), e))?;
    let run = Process::new(bin)
        .args(&[input.display().to_string(), output.display().to_string()])
        .cwd(work)
        .timeout(Duration::from_secs(60))
        .stdout_cap(1 << 20)
        .run()
        .map_err(|e| ToolError::invalid(format!("running crn2blocks failed: {}", e.message)))?;
    if !run.success {
        return Err(ToolError::invalid(format!("this texture's CRN stream didn't decompress: {}", run.stderr_text())));
    }
    let (width, height, dxt_format) = parse_crn_meta(&run.stderr_text()).ok_or_else(|| {
        ToolError::invalid("crn2blocks didn't say what it produced.").with_remediation("Run crn2blocks by hand on the texture to see what it prints.")
    })?;
    let blocks = std::fs::read(&output).map_err(|e| ToolError::io(output.display(), e))?;
    let _ = std::fs::remove_file(&input);
    let _ = std::fs::remove_file(&output);
    Ok(CrnBlocks { width, height, dxt_format, blocks })
}

/// The codec's stderr line, e.g. `128x64 levels=3 fmt=0 bytesPerBlock=8 bytes=8192`: the size of
/// mip 0 and the DXT format its blocks are in.
pub fn parse_crn_meta(line: &str) -> Option<(u32, u32, u32)> {
    let line = line.lines().rev().find(|l| l.contains("bytesPerBlock"))?;
    let (w, rest) = line.split_once('x')?;
    let width: u32 = w.trim().parse().ok()?;
    let mut height = None;
    let mut fmt = None;
    for part in rest.split_whitespace() {
        if let Some(v) = part.strip_prefix("fmt=") {
            fmt = v.parse().ok();
        } else if height.is_none() {
            height = part.parse().ok();
        }
    }
    Some((width, height?, fmt?))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The converter's stderr line names the size of mip 0 and the DXT format its blocks are in.
    #[test]
    fn parses_the_converters_stderr_line() {
        assert_eq!(parse_crn_meta("noise first\n128x64 levels=3 fmt=2 bytesPerBlock=16 bytes=8192\n"), Some((128, 64, 2)));
        assert_eq!(parse_crn_meta("bad texture info\n"), None);
    }
}
