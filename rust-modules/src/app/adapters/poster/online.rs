//! **Plex online posters** (Settings > Libraries > Under posters > Plex online posters).
//!
//! A row hands such a tile the path [`plx_plex::plex::online_poster_path`] builds: the title's
//! catalog id and the server's own poster path. The store key ([`online_key`]) adds the drawn box,
//! and the demand worker, which recognises the key before any request is built (as it does a
//! collection's fan), asks plex.tv's metadata provider for the title ([`resolve`]) and fetches the
//! poster URL it names through the server's transcoder, the way an absolute cast headshot is
//! fetched. Where Plex has no poster, or cannot be reached, the server's poster stands in.
//!
//! The id -> URL answer is kept on the disk tier under its own baked key, so a poster costs one
//! provider read a day, not one a launch. No URL or credential is logged.

use plx_plex::plex::parse_online_poster_path;
use std::os::raw::c_int;

/// The store key prefix of an online poster. Not a server path, and it carries no token.
pub(super) const KEY_PREFIX: &str = "/plx/online-key/";
/// The disk tier's names for an id's resolved poster URL (`classify_baked`).
const URL_NAMESPACE: &str = "plex-online";
const URL_KIND: &str = "poster-url.1";

/// The store key for a row's online poster path drawn in `w` x `h`; `None` for every other path.
pub(super) fn online_key(path: &str, w: c_int, h: c_int) -> Option<String> {
    let (hex, fallback) = parse_online_poster_path(path)?;
    Some(format!("{KEY_PREFIX}{hex}/{w}x{h}{fallback}"))
}

/// An [`online_key`] taken apart.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Key<'a> {
    pub hex: &'a str,
    pub w: c_int,
    pub h: c_int,
    /// The server's own poster path; empty when the row had none.
    pub fallback: &'a str,
}

pub(super) fn parse_key(key: &str) -> Option<Key<'_>> {
    let rest = key.strip_prefix(KEY_PREFIX)?;
    let hex = rest.get(..24)?;
    let rest = rest.get(24..)?.strip_prefix('/')?;
    let (dims, fallback) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    let (w, h) = dims.split_once('x')?;
    Some(Key { hex, w: w.parse().ok()?, h: h.parse().ok()?, fallback })
}

/// What plex.tv said of an id.
pub(super) enum Resolved {
    /// The poster's absolute URL.
    Url(String),
    /// Plex has no poster for it (or no such title): the server's stands in, for good.
    Absent,
    /// No answer now (offline, no account credential): the server's stands in, and it is asked again.
    Transient,
}

/// WORKER: the online poster URL of catalog id `hex`. A disk answer younger than a day is used as
/// it is; an older one is asked again and kept when plex.tv does not answer.
pub(super) fn resolve(hex: &str, cache_gen: u64) -> Resolved {
    let disk = plx_platform::imgcache::classify_baked(URL_NAMESPACE, URL_KIND, hex, "0", 1, 1);
    let cached = disk.as_ref().and_then(|k| plx_platform::imgcache::read_at(cache_gen, k));
    let known = cached.as_ref().and_then(|c| String::from_utf8(c.bytes.clone()).ok()).filter(|u| u.starts_with("https://"));
    if let (Some(url), Some(c)) = (&known, &cached) {
        if !c.stale {
            return Resolved::Url(url.clone());
        }
    }
    match ask(hex) {
        Resolved::Url(url) => {
            if let Some(k) = &disk {
                plx_platform::imgcache::write_at(cache_gen, k, url.as_bytes());
            }
            Resolved::Url(url)
        }
        Resolved::Transient => known.map_or(Resolved::Transient, Resolved::Url),
        Resolved::Absent => Resolved::Absent,
    }
}

/// WORKER: one `GET discover /library/metadata/<hex>` as the seated profile's plex.tv identity.
fn ask(hex: &str) -> Resolved {
    use plx_plex::plex::watchlist::WatchlistError;
    let profile = plx_plex::plex::session::current_snapshot();
    let Some(credential) = profile.user.as_ref().and_then(plx_plex::plex::session::plex_tv_credential) else {
        return Resolved::Transient;
    };
    let client_id = plx_plex::plex::session::peek().client_id.clone();
    if client_id.is_empty() {
        return Resolved::Transient;
    }
    let account = plx_plex::plex::account::AccountClient::new(&client_id, Some(&credential));
    let mut transport = account.watchlist_transport();
    let (answer, why) = match account.catalog_item(hex, &mut transport) {
        Ok(item) => match item.item.thumb.filter(|u| u.starts_with("https://")) {
            Some(url) => (Resolved::Url(url), "url".to_string()),
            None => (Resolved::Absent, "no poster".to_string()),
        },
        Err(e @ (WatchlistError::NotFound | WatchlistError::BadRequest)) => (Resolved::Absent, format!("{e:?}")),
        Err(e) => (Resolved::Transient, format!("{e:?}")),
    };
    note(hex, &why);
    answer
}

/// The first [`NOTE_CAP`] lookups' outcomes in the event log (the catalog id's head and the
/// answer's kind; never a URL or a credential), so a tile that keeps the server's poster says why.
fn note(hex: &str, why: &str) {
    use std::sync::atomic::{AtomicU32, Ordering};
    const NOTE_CAP: u32 = 400;
    static NOTED: AtomicU32 = AtomicU32::new(0);
    if NOTED.fetch_add(1, Ordering::Relaxed) < NOTE_CAP {
        let why: String = why.chars().take(60).collect();
        plx_base::eventlog::log(&format!("posters: online {} -> {why}", hex.get(..8).unwrap_or(hex)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_online_key_carries_the_id_the_box_and_the_servers_poster() {
        let path = plx_plex::plex::online_poster_path(&[0x5d; 12], "/library/metadata/7/thumb/9");
        let key = online_key(&path, 250, 375).unwrap();
        assert_eq!(key, "/plx/online-key/5d5d5d5d5d5d5d5d5d5d5d5d/250x375/library/metadata/7/thumb/9");
        assert_eq!(parse_key(&key), Some(Key {
            hex: "5d5d5d5d5d5d5d5d5d5d5d5d", w: 250, h: 375, fallback: "/library/metadata/7/thumb/9",
        }));
        let bare = online_key(&plx_plex::plex::online_poster_path(&[1; 12], ""), 10, 20).unwrap();
        assert_eq!(parse_key(&bare).map(|k| k.fallback), Some(""));
        assert_eq!(online_key("/library/metadata/7/thumb/9", 250, 375), None);
        assert_eq!(parse_key("/photo/:/transcode?url=x"), None);
    }
}
