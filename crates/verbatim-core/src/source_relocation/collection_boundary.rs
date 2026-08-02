use std::path::{Path, PathBuf};

use anyhow::Result;
use rusqlite::Transaction;

use crate::collection::CollectionRootKind;

pub(super) fn collection_covering_path(
    tx: &Transaction<'_>,
    path: &Path,
) -> Result<Option<String>> {
    let mut statement = tx.prepare(
        "SELECT collection_name, canonical_path, kind
         FROM collection_roots WHERE canonical_path IS NOT NULL ORDER BY collection_name, path",
    )?;
    let roots = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                PathBuf::from(row.get::<_, String>(1)?),
                CollectionRootKind::from_storage_str(&row.get::<_, String>(2)?),
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(roots.into_iter().find_map(|(collection, root, kind)| {
        let covered = path == root
            || matches!(
                kind,
                CollectionRootKind::Directory | CollectionRootKind::SymlinkDirectory
            ) && path.starts_with(&root);
        covered.then_some(collection)
    }))
}
