//! The asset source a layer's document is fetched over.
//!
//! Three kinds of layer share this shape: several may be up at once, each
//! holds one document from its own URL, and each may be refetched.
//! [`crate::overlays`] and [`crate::ephemeris`] are the two; this module holds
//! the plumbing they share — a scheme Bevy's asset server fetches through,
//! which resolves a *slot* rather than a path.
//!
//! **Asset source.** Fetching is asynchronous and platform-shaped (a task
//! thread natively, `fetch()` in the browser), and Bevy already owns both.
//! Registering a scheme gets this for free on both targets and returns a
//! typed asset with reference counting and a load state — the same trick
//! [`crate::imagery`] uses for tiles.
//!
//! **Slot rather than URL.** A reader is built once and outlives every
//! `World`, so it cannot hold anything borrowed from one; a URL is also not a
//! path — its scheme, query and fragment would be mangled by one. A layer is
//! given a slot, the slot holds the URL in a table shared by both sides, and
//! the path is `{slot}/{generation}.{extension}`.
//!
//! **Generation counter.** Refreshing must defeat two caches: Bevy keys its
//! asset cache by path, and the browser keys its own by URL, so re-requesting
//! the same path or URL is answered from cache. Both are bypassed by the same
//! counter — the path carries it, so the asset cache sees a new path, and from
//! the second fetch onward the request carries it too, so the HTTP cache sees
//! a new URL. A layer that never refreshes never gets the extra parameter, so
//! a signed or otherwise parameter-sensitive URL still works.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use bevy::asset::io::web::WebAssetReader;
use bevy::asset::io::{AssetReader, AssetReaderError, AssetSourceBuilder, PathStream, Reader};

/// The URL behind each slot, shared with a reader that lives outside the
/// `World`.
pub type SharedUrls = Arc<RwLock<HashMap<u64, String>>>;

/// The query parameter a refresh adds to get past the HTTP cache. Named rather
/// than a bare `t` so a server log says where it came from.
const CACHE_BUSTER: &str = "_terramenta";

/// The path one slot's document is fetched over, at one generation.
///
/// The extension is what picks the loader, so it is the caller's: a scheme
/// serves whatever its layers hold, and two schemes registered from here need
/// not agree about what a document is.
pub fn asset_path(scheme: &str, slot: u64, generation: u32, extension: &str) -> String {
    format!("{scheme}://{slot}/{generation}.{extension}")
}

/// Builds a source that serves [`asset_path`] by fetching whatever URL that
/// slot holds.
///
/// Register it before `AssetPlugin` builds — an asset source can only be added
/// while the asset server does not yet exist.
pub fn source(urls: SharedUrls) -> AssetSourceBuilder {
    AssetSourceBuilder::new(move || {
        Box::new(SlotReader {
            urls: urls.clone(),
            http: WebAssetReader::Http,
            https: WebAssetReader::Https,
        })
    })
}

/// Serves `{slot}/{generation}.{extension}` out of a table of URLs.
struct SlotReader {
    urls: SharedUrls,
    http: WebAssetReader,
    https: WebAssetReader,
}

/// Recovers the slot and generation a path refers to.
fn parse_path(path: &Path) -> Option<(u64, u32)> {
    let mut segments = path.to_str()?.split('/');
    let slot: u64 = segments.next()?.parse().ok()?;
    let generation: u32 = segments.next()?.split('.').next()?.parse().ok()?;
    Some((slot, generation))
}

/// Adds the cache-busting parameter a refetch needs — and nothing at all to a
/// first fetch, so a URL that cannot take an extra parameter still works for
/// every layer that is not being refreshed.
fn request_url(url: &str, generation: u32) -> String {
    if generation == 0 {
        return url.to_string();
    }
    let separator = if url.contains('?') { '&' } else { '?' };
    format!("{url}{separator}{CACHE_BUSTER}={generation}")
}

impl AssetReader for SlotReader {
    async fn read<'a>(&'a self, path: &'a Path) -> Result<impl Reader + 'a, AssetReaderError> {
        let not_found = || AssetReaderError::NotFound(path.to_path_buf());
        let (slot, generation) = parse_path(path).ok_or_else(not_found)?;

        // Built and the lock released before awaiting, so the guard is never
        // held across a suspension point.
        let url = {
            let urls = self.urls.read().map_err(|_| not_found())?;
            request_url(urls.get(&slot).ok_or_else(not_found)?, generation)
        };

        // Bevy's reader prepends the scheme itself, so hand it the remainder.
        let (reader, remainder) = match url.split_once("://") {
            Some(("https", rest)) => (&self.https, rest),
            Some(("http", rest)) => (&self.http, rest),
            _ => return Err(AssetReaderError::NotFound(PathBuf::from(url))),
        };
        reader.read(Path::new(remainder)).await
    }

    async fn read_meta<'a>(&'a self, path: &'a Path) -> Result<impl Reader + 'a, AssetReaderError> {
        Err::<Box<dyn Reader>, _>(AssetReaderError::NotFound(path.to_path_buf()))
    }

    async fn is_directory<'a>(&'a self, _path: &'a Path) -> Result<bool, AssetReaderError> {
        Ok(false)
    }

    async fn read_directory<'a>(
        &'a self,
        path: &'a Path,
    ) -> Result<Box<PathStream>, AssetReaderError> {
        Err(AssetReaderError::NotFound(path.to_path_buf()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_names_a_slot_and_a_generation() {
        assert_eq!(
            asset_path("geojson", 3, 7, "geojson"),
            "geojson://3/7.geojson"
        );
        assert_eq!(parse_path(Path::new("3/7.geojson")), Some((3, 7)));
        assert_eq!(parse_path(Path::new("4/0.omm")), Some((4, 0)));
        assert_eq!(parse_path(Path::new("nonsense")), None);
    }

    #[test]
    fn only_a_refetch_carries_the_cache_buster() {
        let url = "https://example.org/all_hour.geojson";
        assert_eq!(request_url(url, 0), url);
        assert_eq!(request_url(url, 2), format!("{url}?{CACHE_BUSTER}=2"));
        assert_eq!(
            request_url("https://example.org/feed?format=geojson", 1),
            format!("https://example.org/feed?format=geojson&{CACHE_BUSTER}=1")
        );
    }
}
