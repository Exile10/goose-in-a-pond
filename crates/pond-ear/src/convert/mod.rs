//! ONNX in, `.vmw` out: our own protobuf reader and one converter per architecture, so a freshly
//! downloaded model becomes engine weights on the machine itself, with no Python anywhere.

pub mod kws;
pub mod proto;
pub mod resnet;
pub mod sha256;
pub mod silero;
pub mod vmw;

use std::io::Read;
use std::path::{Path, PathBuf};

use crate::engine::model::{Model, MAGIC};
use crate::Error;
pub use vmw::Aligned;

/// Convert an ONNX model to `.vmw` bytes, recognising its architecture from the graph.
pub fn convert_onnx(onnx: &[u8]) -> Result<Vec<u8>, String> {
    let sha = sha256::sha256(onnx);
    let g = proto::parse_onnx(onnx)?;
    let has = |op: &str| g.nodes.iter().any(|n| n.op == op);
    let depthwise = g
        .nodes
        .iter()
        .any(|n| n.op == "Conv" && n.int("group").unwrap_or(1) > 1);
    let silero = g.nodes.iter().any(|n| {
        n.op == "If"
            && n.attrs.values().any(|a| {
                a.g.as_ref()
                    .is_some_and(|b| b.nodes.iter().any(|x| x.op == "LSTM"))
            })
    });
    let vmw = if silero {
        silero::convert(&g, sha)?
    } else if has("GlobalAveragePool") && depthwise {
        kws::convert(&g, sha)?
    } else if has("Conv") && has("Gemm") && !has("BatchNormalization") {
        resnet::convert(&g, sha)?
    } else {
        return Err("unrecognised model architecture".into());
    };
    Ok(vmw.to_bytes())
}

/// `<stem>.onnx` -> `<stem>.vmw` beside it, reconverting only when the ONNX or the converter changed.
pub fn ensure_vmw(onnx_path: &Path) -> Result<PathBuf, Error> {
    let out = onnx_path.with_extension("vmw");
    let onnx = std::fs::read(onnx_path).map_err(|e| Error::io(onnx_path, e))?;
    let sha = sha256::sha256(&onnx);
    if let Ok(existing) = std::fs::read(&out) {
        let converter = existing
            .get(36..40)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
        if existing.len() > 72 && existing[40..72] == sha && converter == Some(vmw::CONVERTER) {
            return Ok(out);
        }
    }
    let bytes = convert_onnx(&onnx).map_err(Error::Convert)?;
    std::fs::write(&out, bytes).map_err(|e| Error::io(&out, e))?;
    Ok(out)
}

/// A validated `.vmw` held in aligned memory, from wherever it came: an ONNX converted here, a
/// `.vmw` file, or the embedded wake model.
pub struct ModelFile {
    file: Aligned,
}

impl ModelFile {
    pub fn from_onnx(onnx: &[u8]) -> Result<ModelFile, Error> {
        let bytes = convert_onnx(onnx).map_err(Error::Convert)?;
        ModelFile::from_aligned(Aligned::new(&bytes))
    }

    pub fn from_vmw(bytes: &[u8]) -> Result<ModelFile, Error> {
        ModelFile::from_aligned(Aligned::new(bytes))
    }

    /// A `.vmw` (read straight into aligned memory) or an ONNX (converted), told apart by magic.
    pub fn read(path: &Path) -> Result<ModelFile, Error> {
        let mut magic = [0u8; 4];
        let is_vmw = std::fs::File::open(path)
            .and_then(|mut f| f.read_exact(&mut magic))
            .is_ok()
            && &magic == MAGIC;
        if is_vmw {
            return ModelFile::from_aligned(Aligned::read(path).map_err(|e| Error::io(path, e))?);
        }
        let onnx = std::fs::read(path).map_err(|e| Error::io(path, e))?;
        ModelFile::from_onnx(&onnx)
    }

    fn from_aligned(file: Aligned) -> Result<ModelFile, Error> {
        Model::parse(file.bytes()).map_err(Error::Format)?;
        Ok(ModelFile { file })
    }

    /// The parsed view; parsing is O(ops) and was validated when loaded.
    pub fn model(&self) -> Model<'_> {
        match Model::parse(self.file.bytes()) {
            Ok(m) => m,
            Err(_) => unreachable!("validated when loaded"),
        }
    }

    pub fn bytes(&self) -> &[u8] {
        self.file.bytes()
    }

    /// SHA-256 of the ONNX this was converted from.
    pub fn source_sha256(&self) -> [u8; 32] {
        self.model().source_sha256
    }
}
