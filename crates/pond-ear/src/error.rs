use std::fmt;
use std::path::{Path, PathBuf};

use crate::engine::{exec, model};

#[derive(Debug)]
pub enum Error {
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    /// The ONNX graph is not one the converters know.
    Convert(String),
    /// The `.vmw` failed validation.
    Format(model::Error),
    /// The engine cannot plan this model for the input length it was given.
    Plan(exec::Error),
    /// A valid model, but the wrong kind for the part it was given to.
    WrongModel(String),
}

impl Error {
    pub(crate) fn io(path: &Path, source: std::io::Error) -> Error {
        Error::Io {
            path: path.to_path_buf(),
            source,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io { path, source } => write!(f, "{}: {source}", path.display()),
            Error::Convert(e) => write!(f, "model conversion failed: {e}"),
            Error::Format(e) => write!(f, "invalid .vmw: {e:?}"),
            Error::Plan(e) => write!(f, "cannot plan the model: {e:?}"),
            Error::WrongModel(e) => f.write_str(e),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}
