pub mod realm_of_darkness;
pub mod swf_rip;
pub mod url_cloner;

use anyhow::{anyhow, Result};

/// One sound pulled out of a cloned source.
pub struct ImportedButton {
    pub label: String,
    pub file: String,
}

pub struct ImportResult {
    pub tab_name: String,
    pub buttons: Vec<ImportedButton>,
}

/// Called with a human-readable progress line as the importer works.
pub type ProgressFn<'a> = dyn Fn(&str) + 'a;
/// Polled periodically; return `false` to cancel the import ASAP.
pub type RunningFn<'a> = dyn Fn() -> bool + 'a;

/// An importer that can turn some external source (a URL, typically) into
/// a set of downloaded sound files. Importers are stateless and must not
/// touch application state directly -- they run on a background thread and
/// hand back plain data for the UI thread to turn into a `TabModel`. Same
/// separation `base.py`/`dispatcher.py` enforced.
pub trait Importer: Send + Sync {
    fn can_handle(&self, source: &str) -> bool;
    fn import_from(&self, source: &str, progress: &ProgressFn, running: &RunningFn) -> Result<ImportResult>;
}

/// Where downloaded sounds land: `<data dir>/downloaded-sounds/<slug>/`.
pub fn downloads_root() -> std::path::PathBuf {
    crate::persistence::data_dir().join("downloaded-sounds")
}

pub fn import_from_source(source: &str, progress: &ProgressFn, running: &RunningFn) -> Result<ImportResult> {
    let importers: Vec<Box<dyn Importer>> =
        vec![Box::new(realm_of_darkness::RealmOfDarknessImporter), Box::new(url_cloner::UrlCloner)];

    for importer in &importers {
        if importer.can_handle(source) {
            return importer.import_from(source, progress, running);
        }
    }

    Err(anyhow!("No importer available for source: {source}"))
}
