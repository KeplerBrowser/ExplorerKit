//! Read the committed first-party map from a named WebKit store.

use std::{
    ffi::OsStr,
    fs,
    io::{ErrorKind, Read},
    os::unix::fs::MetadataExt,
    path::{Component, Path, PathBuf},
    time::Duration,
};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use objc2_foundation::{
    NSBundle, NSFileManager, NSProcessInfo, NSSearchPathDirectory, NSSearchPathDomainMask,
    NSString, NSUserDefaults, NSUUID,
};
use rusqlite::{Connection, OpenFlags};
use sha2::{Digest, Sha256};
use url::Url;

use crate::site_data::{SiteDataLocalStorage, SiteDataOrigin};

const ITEM_TABLE: &str =
    "ItemTable (key TEXT UNIQUE ON CONFLICT REPLACE, value BLOB NOT NULL ON CONFLICT FAIL)";

pub(super) struct NamedStore {
    library: PathBuf,
    app_name: String,
    identifier: String,
}

impl NamedStore {
    pub(super) fn new(identifier: &NSUUID) -> Result<Self, ()> {
        // Older WebKit versions can deliberately select a different layout
        // with this debug override, leaving a stale database in the default one.
        if NSUserDefaults::standardUserDefaults()
            .objectForKey(&NSString::from_str("WebKitDebugUnifiedOriginStorageLevel"))
            .is_some()
        {
            return Err(());
        }
        let library = NSFileManager::defaultManager()
            .URLForDirectory_inDomain_appropriateForURL_create_error(
                NSSearchPathDirectory::LibraryDirectory,
                NSSearchPathDomainMask::UserDomainMask,
                None,
                false,
            )
            .map_err(|_| ())?;
        let process = NSProcessInfo::processInfo().processName().to_string();
        let bundle = NSBundle::mainBundle()
            .bundleIdentifier()
            .map(|id| id.to_string());
        let app_name = match bundle {
            Some(id) if !(id == "com.apple.Safari" && process == "SafariForWebKitDevelopment") => {
                id
            }
            _ => process,
        };
        let mut components = Path::new(&app_name).components();
        if !matches!(
            (components.next(), components.next()),
            (Some(Component::Normal(_)), None)
        ) {
            return Err(());
        }
        Ok(Self {
            library: PathBuf::from(library.path().ok_or(())?.to_string()),
            app_name,
            identifier: identifier.UUIDString().to_string(),
        })
    }

    pub(super) fn open(self) -> Result<Option<Storage>, ()> {
        let Some(webkit) = child_directory(&self.library, "WebKit")? else {
            return Ok(None);
        };
        let app = child_directory(&webkit, &self.app_name)?;
        let lowercase = self.identifier.to_ascii_lowercase();
        let mut named_store = None;
        for parent in [Some(webkit), app].into_iter().flatten() {
            let Some(stores) = child_directory(&parent, "WebsiteDataStore")? else {
                continue;
            };
            for id in [&self.identifier, &lowercase] {
                if let Some(path) = child_directory(&stores, id)? {
                    let meta = fs::metadata(&path).map_err(|_| ())?;
                    if let Some((_, device, inode)) = &named_store {
                        if (*device, *inode) != (meta.dev(), meta.ino()) {
                            return Err(());
                        }
                    } else {
                        named_store = Some((path, meta.dev(), meta.ino()));
                    }
                }
            }
        }
        let Some((root, ..)) = named_store else {
            return Ok(None);
        };
        let legacy = child_directory(&root, "LocalStorage")?;
        let unified = if let Some(origins) = child_directory(&root, "Origins")? {
            let Some(salt) = child_file(&origins, "salt")? else {
                return Ok(None);
            };
            let salt = read_small(&salt, 8)?.try_into().map_err(|_| ())?;
            Some((origins, salt))
        } else {
            None
        };
        if unified.is_none() && legacy.is_none() {
            return Ok(None);
        }
        Ok(Some(Storage { unified, legacy }))
    }
}

pub(super) struct Storage {
    unified: Option<(PathBuf, [u8; 8])>,
    legacy: Option<PathBuf>,
}

impl Storage {
    pub(super) fn capture(&self, origin: SiteDataOrigin) -> Result<SiteDataLocalStorage, ()> {
        let canonical = origin.serialize();
        let url = Url::parse(&canonical).map_err(|_| ())?;
        let unified = self.unified_file(&canonical, &url)?;
        let legacy = self.legacy_file(&url)?;
        let entries = match (unified, legacy) {
            (Some(path), None) | (None, Some(path)) => read_entries(&path)?,
            // The engine can migrate a legacy file into the unified layout.
            // With two copies there is no public way to identify the active one.
            (Some(_), Some(_)) => return Err(()),
            (None, None) if self.unified.is_some() => Vec::new(),
            (None, None) => return Err(()),
        };
        Ok(SiteDataLocalStorage { origin, entries })
    }

    fn unified_file(&self, canonical: &str, url: &Url) -> Result<Option<PathBuf>, ()> {
        let Some((root, salt)) = &self.unified else {
            return Ok(None);
        };
        // WebKit hashes each component of ClientOrigin. First-party top and
        // client origins are identical and use the native canonical tuple.
        let mut hash = Sha256::new();
        hash.update(canonical.as_bytes());
        hash.update(salt);
        let digest = URL_SAFE_NO_PAD.encode(hash.finalize());
        let Some(top) = child_directory(root, &digest)? else {
            return Ok(None);
        };
        let Some(client) = child_directory(&top, &digest)? else {
            return Ok(None);
        };
        let Some(storage) = child_directory(&client, "LocalStorage")? else {
            return Ok(None);
        };
        let Some(path) = child_file(&storage, "localstorage.sqlite3")? else {
            if fs::read_dir(&storage).map_err(|_| ())?.next().is_some() {
                return Err(());
            }
            return Ok(None);
        };
        let marker = child_file(&client, "origin")?.ok_or(())?;
        verify_origin(&read_small(&marker, 4096)?, url)?;
        Ok(Some(path))
    }

    fn legacy_file(&self, url: &Url) -> Result<Option<PathBuf>, ()> {
        let Some(root) = &self.legacy else {
            return Ok(None);
        };
        let host = url.host_str().ok_or(())?;
        // SecurityOriginData::databaseIdentifier uses WebKit's filename
        // escapes. Canonical URL hosts are ASCII, including bracketed IPv6.
        let escaped: String = host
            .bytes()
            .map(|byte| {
                if byte.is_ascii_control() || b" \"%*/:<>?\\|".contains(&byte) {
                    format!("%{byte:02X}")
                } else {
                    char::from(byte).to_string()
                }
            })
            .collect();
        let name = format!(
            "{}_{escaped}_{}.localstorage",
            url.scheme(),
            url.port().unwrap_or(0)
        );
        let path = child_file(root, &name)?;
        // The legacy name conflates an omitted port with explicit port zero.
        if path.is_some() && url.port().unwrap_or(0) == 0 {
            return Err(());
        }
        if path.is_none() {
            for suffix in ["-wal", "-shm", "-journal"] {
                if child_file(root, &format!("{name}{suffix}"))?.is_some() {
                    return Err(());
                }
            }
        }
        Ok(path)
    }
}

fn child(path: &Path, name: impl AsRef<OsStr>, is_directory: bool) -> Result<Option<PathBuf>, ()> {
    let path = path.join(name.as_ref());
    match fs::symlink_metadata(&path) {
        Ok(meta) if is_directory && meta.is_dir() => Ok(Some(path)),
        Ok(meta) if !is_directory && meta.is_file() => Ok(Some(path)),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        _ => Err(()),
    }
}

fn child_directory(path: &Path, name: &str) -> Result<Option<PathBuf>, ()> {
    child(path, name, true)
}

fn child_file(path: &Path, name: &str) -> Result<Option<PathBuf>, ()> {
    child(path, name, false)
}

fn read_small(path: &Path, limit: usize) -> Result<Vec<u8>, ()> {
    let mut bytes = Vec::new();
    fs::File::open(path)
        .map_err(|_| ())?
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| ())?;
    (bytes.len() <= limit).then_some(bytes).ok_or(())
}

fn take<'a>(bytes: &mut &'a [u8], count: usize) -> Result<&'a [u8], ()> {
    let (value, tail) = bytes.split_at_checked(count).ok_or(())?;
    *bytes = tail;
    Ok(value)
}

fn verify_origin(mut bytes: &[u8], url: &Url) -> Result<(), ()> {
    // The origin file encodes top and client SecurityOriginData, each as
    // two WTF strings and an optional native-endian u16 port.
    for _ in 0..2 {
        for text in [url.scheme(), url.host_str().ok_or(())?] {
            let length =
                u32::from_ne_bytes(take(&mut bytes, 4)?.try_into().map_err(|_| ())?) as usize;
            if length != text.len() {
                return Err(());
            }
            match take(&mut bytes, 1)? {
                [1] if take(&mut bytes, length)? == text.as_bytes() => {}
                [0] if take(&mut bytes, length * 2)?
                    .chunks_exact(2)
                    .zip(text.bytes())
                    .all(|(unit, ascii)| {
                        u16::from_ne_bytes([unit[0], unit[1]]) == u16::from(ascii)
                    }) => {}
                _ => return Err(()),
            }
        }
        match (take(&mut bytes, 1)?, url.port()) {
            ([0], None) => {}
            ([1], Some(port)) if take(&mut bytes, 2)? == port.to_ne_bytes() => {}
            _ => return Err(()),
        }
    }
    bytes.is_empty().then_some(()).ok_or(())
}

fn read_entries(path: &Path) -> Result<Vec<(String, String)>, ()> {
    // Let SQLite coordinate its read with WebKit and include committed WAL frames.
    let parent = path.parent().ok_or(())?;
    let name = path.file_name().and_then(OsStr::to_str).ok_or(())?;
    for suffix in ["-wal", "-shm", "-journal"] {
        child_file(parent, &format!("{name}{suffix}"))?;
    }
    let mut conn =
        Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(|_| ())?;
    conn.busy_timeout(Duration::ZERO).map_err(|_| ())?;
    let snapshot = conn.transaction().map_err(|_| ())?;
    // WebKit itself checks this schema before opening ItemTable.
    let schema: String = snapshot
        .query_row(
            "SELECT sql FROM sqlite_schema WHERE type = 'table' AND name = 'ItemTable'",
            [],
            |row| row.get(0),
        )
        .map_err(|_| ())?;
    if schema.strip_prefix("CREATE TABLE ").is_none_or(|sql| {
        sql != ITEM_TABLE && sql.strip_prefix("IF NOT EXISTS ") != Some(ITEM_TABLE)
    }) {
        return Err(());
    }
    let entries = {
        let mut statement = snapshot
            .prepare("SELECT key, value FROM ItemTable")
            .map_err(|_| ())?;
        let mut rows = statement.query([]).map_err(|_| ())?;
        let mut entries = Vec::new();
        while let Some(row) = rows.next().map_err(|_| ())? {
            let key: String = row.get(0).map_err(|_| ())?;
            let value: &[u8] = row.get_ref(1).map_err(|_| ())?.as_blob().map_err(|_| ())?;
            let units = value.chunks_exact(2);
            if !units.remainder().is_empty() {
                return Err(());
            }
            let units: Vec<_> = units
                .map(|unit| u16::from_ne_bytes([unit[0], unit[1]]))
                .collect();
            entries.push((key, String::from_utf16(&units).map_err(|_| ())?));
        }
        entries
    };
    snapshot.commit().map_err(|_| ())?;
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "explorerkit-committed-storage-{}",
                NSUUID::new().UUIDString()
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[test]
    fn snapshots_include_committed_wal_frames_and_reject_wrong_schema() {
        let fixture = Fixture::new();
        let database = fixture.0.join("localstorage.sqlite3");
        let mut writer = Connection::open(&database).unwrap();
        writer
            .execute_batch(
                "PRAGMA journal_mode = WAL;
             CREATE TABLE ItemTable (key TEXT UNIQUE ON CONFLICT REPLACE, value BLOB NOT NULL ON CONFLICT FAIL);
             INSERT INTO ItemTable VALUES ('deleted', X'6100');",
            )
            .unwrap();
        let transaction = writer.transaction().unwrap();
        transaction
            .execute_batch(
                "DELETE FROM ItemTable;
             INSERT INTO ItemTable VALUES ('鍵', X'EA963ED8EADD0000'), ('', X'');",
            )
            .unwrap();
        assert_eq!(
            read_entries(&database).unwrap(),
            [("deleted".into(), "a".into())]
        );
        transaction.commit().unwrap();

        let mut entries = read_entries(&database).unwrap();
        entries.sort();
        assert_eq!(
            entries,
            [("".into(), "".into()), ("鍵".into(), "雪🧪\0".into())]
        );

        writer
            .execute_batch("INSERT INTO ItemTable VALUES ('invalid', X'00D8');")
            .unwrap();
        assert!(read_entries(&database).is_err());
        writer
            .execute_batch("DROP TABLE ItemTable; CREATE TABLE ItemTable (key TEXT, value BLOB);")
            .unwrap();
        assert!(read_entries(&database).is_err());
    }

    #[test]
    fn origin_marker_requires_exact_top_and_client_tuples() {
        let native = |port: Option<u16>| {
            let mut bytes = vec![4, 0, 0, 0, 1];
            bytes.extend_from_slice(b"http");
            bytes.extend_from_slice(&[5, 0, 0, 0, 1]);
            bytes.extend_from_slice(b"[::1]");
            bytes.push(u8::from(port.is_some()));
            if let Some(port) = port {
                bytes.extend_from_slice(&port.to_ne_bytes());
            }
            bytes
        };
        let url = Url::parse("http://[::1]:80/").unwrap();
        let correct = [native(None), native(None)].concat();
        assert!(verify_origin(&correct, &url).is_ok());
        assert!(verify_origin(&[native(Some(8000)), native(None)].concat(), &url).is_err());
        assert!(verify_origin(&[native(None), native(Some(8000))].concat(), &url).is_err());
        assert!(verify_origin(&correct[..correct.len() - 1], &url).is_err());
    }

    #[test]
    fn absence_requires_a_recognized_layout_and_bad_paths_fail_closed() {
        let fixture = Fixture::new();
        let origin = || SiteDataOrigin::parse("https://example.test").unwrap();
        let unknown = Storage {
            unified: None,
            legacy: None,
        };
        assert!(unknown.capture(origin()).is_err());

        let salt = [7; 8];
        let known = Storage {
            unified: Some((fixture.0.clone(), salt)),
            legacy: None,
        };
        assert!(known.capture(origin()).unwrap().entries.is_empty());

        let mut hash = Sha256::new();
        hash.update(origin().serialize());
        hash.update(salt);
        let digest = URL_SAFE_NO_PAD.encode(hash.finalize());
        let directory = fixture.0.join(&digest).join(&digest).join("LocalStorage");
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("unknown.sqlite"), []).unwrap();
        assert!(known.capture(origin()).is_err());
    }

    #[test]
    fn legacy_default_port_file_cannot_identify_the_exact_origin() {
        let fixture = Fixture::new();
        let storage = Storage {
            unified: None,
            legacy: Some(fixture.0.clone()),
        };
        for port in [0, 8443] {
            let path = fixture
                .0
                .join(format!("https_example.test_{port}.localstorage"));
            Connection::open(&path)
                .unwrap()
                .execute_batch(&format!(
                    "CREATE TABLE {ITEM_TABLE}; INSERT INTO ItemTable VALUES ('token', X'6100');"
                ))
                .unwrap();
        }
        for url in ["https://example.test", "https://example.test:0"] {
            assert!(storage
                .capture(SiteDataOrigin::parse(url).unwrap())
                .is_err());
        }
        let exact = storage
            .capture(SiteDataOrigin::parse("https://example.test:8443").unwrap())
            .unwrap();
        assert_eq!(exact.entries, [("token".into(), "a".into())]);
    }

    #[test]
    fn distinct_store_roots_with_the_same_uuid_are_ambiguous() {
        let fixture = Fixture::new();
        let id = NSUUID::new().UUIDString().to_string();
        let webkit = fixture.0.join("WebKit");
        for base in [&webkit, &webkit.join("Fixture")] {
            fs::create_dir_all(base.join("WebsiteDataStore").join(&id)).unwrap();
        }
        let candidate = NamedStore {
            library: fixture.0.clone(),
            app_name: "Fixture".into(),
            identifier: id,
        };
        assert!(candidate.open().is_err());
    }
}
