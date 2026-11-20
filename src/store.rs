//! Where models live on this machine, shared by the CLI and the GUI.

use std::path::{Path, PathBuf};

use crate::{model_sha256, Error, Model, MODELS};

pub fn exe_dir() -> Option<PathBuf> {
    Some(std::env::current_exe().ok()?.parent()?.to_path_buf())
}

/// Per-user data folder; a `portable` file next to the exe keeps everything beside it.
pub fn data_dir() -> PathBuf {
    if let Some(d) = exe_dir().filter(|d| d.join("portable").exists()) {
        return d;
    }
    let base = if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(".local/share")))
    };
    base.unwrap_or_else(|| PathBuf::from(".")).join("hamzip")
}

/// An installed published model: next to the program, then in the data folder.
pub fn find_installed(name: &str) -> Option<PathBuf> {
    exe_dir()
        .into_iter()
        .chain([data_dir()])
        .map(|d| d.join(format!("{name}.hzm")))
        .find(|p| p.is_file())
}

pub fn not_installed(name: &str) -> Error {
    let other = MODELS
        .iter()
        .map(|(n, _)| *n)
        .find(|n| *n != name && find_installed(n).is_some());
    let hint = other
        .map(|o| format!(", or pass --model {o}"))
        .unwrap_or_default();
    Error::Io(format!(
        "model {name} is not installed: run `hamzip model download {name}`{hint}"
    ))
}

/// Loads a model file, and when it should be a published model, checks it is: a
/// damaged or rebuilt file would code messages nobody else can read.
pub fn load_checked(path: &Path, name: Option<&str>) -> Result<Model, Error> {
    let m = Model::load(path).map_err(|e| match e {
        Error::Io(_) if name.is_none() && !path.exists() => Error::Io(format!(
            "{}: no such file, and not a published model's name (see `hamzip model list`)",
            path.display()
        )),
        Error::Io(_) => e,
        e => Error::Io(format!("{}: {e}", path.display())),
    })?;
    if let Some(name) = name {
        let sha: String = m.sha256.iter().map(|b| format!("{b:02x}")).collect();
        if model_sha256(name) != Some(sha.as_str()) {
            return Err(Error::Io(format!(
                "{} is not the published {name} (its SHA-256 differs); run `hamzip model \
                 download {name}` to replace it",
                path.display()
            )));
        }
    }
    Ok(m)
}

/// Loads an installed published model by name, checked against its hash.
pub fn load_published(name: &str) -> Result<Model, Error> {
    let path = find_installed(name).ok_or_else(|| not_installed(name))?;
    load_checked(&path, Some(name))
}
