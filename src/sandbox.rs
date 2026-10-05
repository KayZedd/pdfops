//! Optional confinement of file access to one directory, for the MCP server.
//!
//! An agent may be steered by the documents it reads. With a root set, no tool
//! can be talked into reading or writing outside of it.

use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{Result, anyhow, bail};
use serde_json::Value;

static ROOT: OnceLock<PathBuf> = OnceLock::new();

/// Argument names that hold file or directory paths, in any tool.
pub const PATH_KEYS: [&str; 6] = ["input", "inputs", "output", "out_dir", "image", "font"];

/// Confines all later tool calls to `dir`, and resolves relative paths against it.
pub fn set_root(dir: &Path) -> Result<()> {
    let root = dir
        .canonicalize()
        .map_err(|e| anyhow!("cannot use {} as root: {e}", dir.display()))?;
    if !root.is_dir() {
        bail!("root {} is not a directory", root.display());
    }
    std::env::set_current_dir(&root)?;
    ROOT.set(root)
        .map_err(|_| anyhow!("the root is already set"))
}

pub fn active() -> bool {
    ROOT.get().is_some()
}

/// The real location `path` refers to, following links in the part that exists.
fn resolve(path: &Path, base: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    };
    // A file about to be created has no real path yet; its nearest existing ancestor has.
    let mut existing = absolute.as_path();
    let mut rest: Vec<&std::ffi::OsStr> = Vec::new();
    while !existing.exists() {
        let (Some(parent), Some(name)) = (existing.parent(), existing.file_name()) else {
            bail!("{} cannot be resolved", path.display());
        };
        rest.push(name);
        existing = parent;
    }
    let mut real = existing.canonicalize()?;
    for name in rest.into_iter().rev() {
        // `..` in the part that does not exist yet could climb back out unnoticed.
        if !matches!(
            Path::new(name).components().next(),
            Some(Component::Normal(_))
        ) {
            bail!("{} cannot be resolved", path.display());
        }
        real.push(name);
    }
    Ok(real)
}

/// Fails unless `path` lies inside `root`, which must be canonical.
pub fn within(root: &Path, path: &Path) -> Result<()> {
    let real = resolve(path, root)?;
    if real.starts_with(root) {
        Ok(())
    } else {
        bail!(
            "{} is outside the allowed directory {}",
            path.display(),
            root.display()
        )
    }
}

/// Fails if a root is set and `path` lies outside it.
pub fn check(path: &Path) -> Result<()> {
    match ROOT.get() {
        Some(root) => within(root, path),
        None => Ok(()),
    }
}

/// Checks every path argument of a tool call.
pub fn check_args(args: &Value) -> Result<()> {
    if !active() {
        return Ok(());
    }
    for key in PATH_KEYS {
        match args.get(key) {
            Some(Value::String(path)) => check(Path::new(path))?,
            Some(Value::Array(paths)) => {
                for path in paths.iter().filter_map(Value::as_str) {
                    check(Path::new(path))?;
                }
            }
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::within;

    #[test]
    fn paths_are_confined_to_the_root() {
        let outer = std::env::temp_dir().join(format!("pdfops-sandbox-{}", std::process::id()));
        let root = outer.join("root");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(outer.join("secret.pdf"), b"x").unwrap();
        let root = root.canonicalize().unwrap();

        assert!(within(&root, "a.pdf".as_ref()).is_ok());
        assert!(within(&root, "sub/new/out.pdf".as_ref()).is_ok());
        assert!(within(&root, &root.join("sub/a.pdf")).is_ok());

        assert!(within(&root, "../secret.pdf".as_ref()).is_err());
        assert!(within(&root, "sub/../../secret.pdf".as_ref()).is_err());
        assert!(within(&root, &outer.join("secret.pdf")).is_err());
        // Not yet existing, but climbing out through a directory that does not exist either.
        assert!(within(&root, "new/../../secret.pdf".as_ref()).is_err());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&outer, root.join("link")).unwrap();
            assert!(within(&root, "link/secret.pdf".as_ref()).is_err());
        }
        std::fs::remove_dir_all(&outer).unwrap();
    }
}
