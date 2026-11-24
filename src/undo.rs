use std::path::PathBuf;

use crate::{format::plural, fsops};

pub enum Undo {
    Moved(Vec<(PathBuf, PathBuf)>),
    Created(Vec<PathBuf>),
    Trashed(Vec<PathBuf>),
}

impl Undo {
    pub fn apply(self) -> Result<String, String> {
        match self {
            Undo::Moved(pairs) => {
                let back: Vec<(PathBuf, PathBuf)> =
                    pairs.iter().map(|(from, to)| (to.clone(), from.clone())).collect();
                fsops::rename_many(&back).map_err(|e| format!("Can't undo: {e}"))?;
                Ok(format!("Undid move/rename of {}", plural(pairs.len(), "item")))
            }
            Undo::Created(paths) => {
                trash::delete_all(&paths).map_err(|e| e.to_string())?;
                Ok(format!("Undid create: {} went to the trash", plural(paths.len(), "item")))
            }
            Undo::Trashed(paths) => restore_from_trash(&paths),
        }
    }
}

fn restore_from_trash(paths: &[PathBuf]) -> Result<String, String> {
    use trash::os_limited;

    let in_trash = os_limited::list().map_err(|e| e.to_string())?;

    let items: Vec<_> = paths
        .iter()
        .filter_map(|path| {
            in_trash.iter().filter(|item| item.original_path() == *path).max_by_key(|item| item.time_deleted).cloned()
        })
        .collect();

    if items.is_empty() {
        return Err("Couldn't find those files in the trash".into());
    }
    let count = items.len();
    os_limited::restore_all(items).map_err(|e| e.to_string())?;
    Ok(format!("Restored {} from the trash", plural(count, "item")))
}
