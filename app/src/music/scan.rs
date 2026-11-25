//! Read the music folder: `Artist/Album (Year)/tracks`. File names and sizes only; no tag and
//! no audio is read in this pass. Plain parsing and a walk with `std::fs`, so it is tested on
//! any machine.
//!
//! - Entries whose name starts with `.`, `@` or `#` are skipped (hidden files, a NAS's
//!   thumbnail and recycle folders). Symbolic links are skipped, so the walk never leaves the
//!   folder.
//! - A file in an album folder is a track when its extension is an audio format, a cover
//!   candidate when it is an image, and counted as another file otherwise. Audio in a folder
//!   one level down (a disc folder) belongs to the album too.
//! - Names that are not UTF-8 are skipped and counted.

use serde::Serialize;
use std::fs;
use std::path::{Component, Path, PathBuf};

/// The audio formats, by file extension.
pub const AUDIO: [&str; 16] = [
    "flac", "mp3", "m4a", "aac", "ogg", "opus", "wav", "wma", "aiff", "aif", "alac", "ape", "wv",
    "dsf", "dff", "mka",
];
/// The image formats a cover can have.
pub const IMAGES: [&str; 6] = ["jpg", "jpeg", "png", "webp", "gif", "bmp"];
/// A cover larger than this is not served.
pub const MAX_COVER_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ScannedTrack {
    /// Relative to the album folder.
    pub file_name: String,
    pub track_number: Option<i32>,
    pub title: Option<String>,
    pub format: String,
    pub size_bytes: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ScannedAlbum {
    pub artist_folder: String,
    pub folder: String,
    pub name: String,
    pub name_key: String,
    pub year: Option<i32>,
    pub tracks: Vec<ScannedTrack>,
    /// Relative to the music root.
    pub cover_path: Option<String>,
    pub other_files: i32,
}

impl ScannedAlbum {
    pub fn size_bytes(&self) -> i64 {
        self.tracks.iter().map(|t| t.size_bytes).sum()
    }

    /// The formats of the tracks, sorted.
    pub fn formats(&self) -> Vec<String> {
        let mut f: Vec<String> = self.tracks.iter().map(|t| t.format.clone()).collect();
        f.sort();
        f.dedup();
        f
    }
}

/// What a walk of the folder found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MusicTree {
    /// Artist folders that hold at least one album folder.
    pub artists: Vec<String>,
    pub albums: Vec<ScannedAlbum>,
    /// Entries that could not be read (a name that is not UTF-8, a folder that failed).
    pub skipped: usize,
}

/// `Riding With the King (2000)` is `Riding With the King` from 2000. `(0000)` means the year
/// is not known; a folder without `(YYYY)` at its end has no year.
pub fn album_name_and_year(folder: &str) -> (String, Option<i32>) {
    let f = folder.trim();
    if let Some(open) = f.rfind('(')
        && let Some(digits) = f[open + 1..].strip_suffix(')')
        && digits.len() == 4
        && digits.bytes().all(|b| b.is_ascii_digit())
    {
        let name = f[..open].trim_end();
        if !name.is_empty() {
            let year: i32 = digits.parse().unwrap_or(0);
            return (
                name.to_string(),
                (1000..=2200).contains(&year).then_some(year),
            );
        }
    }
    (f.to_string(), None)
}

/// The name in lower case with single spaces: two folders with the same key hold the same
/// album.
pub fn name_key(name: &str) -> String {
    name.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// How an artist sorts: lower case, without a leading "The ".
pub fn sort_name(artist: &str) -> String {
    let lower = name_key(artist);
    lower
        .strip_prefix("the ")
        .map(str::to_string)
        .unwrap_or(lower)
}

/// The extension of a file name, lower case.
pub fn extension(file_name: &str) -> Option<String> {
    let (stem, ext) = file_name.rsplit_once('.')?;
    (!stem.is_empty() && !ext.is_empty() && ext.len() <= 5).then(|| ext.to_ascii_lowercase())
}

fn stem(file_name: &str) -> &str {
    match file_name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() && ext.len() <= 5 => stem,
        _ => file_name,
    }
}

fn number(text: &str) -> Option<i32> {
    if text.is_empty() || text.len() > 3 || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

/// The track number and title in a file name. `Artist - Album - 07 - Title.flac` and
/// `07 - Title.flac` use the last number between ` - ` that has a title after it (so an album
/// called `21` is not taken for the number); `07. Title`, `07 Title` and `1-07 Title` (disc
/// and track) use the number in front. `00` is no number.
pub fn track_of(file_name: &str) -> (Option<i32>, Option<String>) {
    let stem = stem(file_name).trim();
    let parts: Vec<&str> = stem.split(" - ").map(str::trim).collect();
    let tidy = |n: i32| (n > 0).then_some(n);
    if parts.len() > 1
        && let Some(at) = (0..parts.len() - 1)
            .rev()
            .find(|i| number(parts[*i]).is_some())
    {
        let title = parts[at + 1..].join(" - ");
        return (
            number(parts[at]).and_then(tidy),
            (!title.is_empty()).then_some(title),
        );
    }
    let digits: String = stem.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() || digits.len() > 3 {
        return (None, (!stem.is_empty()).then(|| stem.to_string()));
    }
    let mut rest = &stem[digits.len()..];
    let mut n = number(&digits);
    // `1-07 Title`: a disc number, then the track.
    if let Some(after) = rest.strip_prefix('-') {
        let second: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
        if !second.is_empty() && second.len() <= 3 {
            n = number(&second);
            rest = &after[second.len()..];
        }
    }
    // The number must be followed by a separator, or be the whole name.
    if !(rest.is_empty() || rest.starts_with([' ', '.', '-', '_'])) {
        return (None, Some(stem.to_string()));
    }
    let title = rest.trim_start_matches([' ', '.', '-', '_']).trim();
    (
        n.and_then(tidy),
        (!title.is_empty()).then(|| title.to_string()),
    )
}

/// The cover among an album folder's images: `cover`, `folder`, `front` or `albumart` by
/// name, else the first image by name.
pub fn pick_cover(images: &[String]) -> Option<String> {
    let stem_of = |name: &str| stem(name).to_ascii_lowercase();
    for wanted in ["cover", "folder", "front", "albumart", "album"] {
        if let Some(found) = images.iter().find(|n| stem_of(n) == wanted) {
            return Some(found.clone());
        }
    }
    images.iter().min().cloned()
}

/// Names the walk passes over: hidden entries and a NAS's own folders.
pub fn skip_name(name: &str) -> bool {
    name.starts_with('.') || name.starts_with('@') || name.starts_with('#')
}

struct Entry {
    name: String,
    path: PathBuf,
    is_dir: bool,
    len: u64,
}

/// The readable entries of a folder, sorted by name. Symbolic links are left out.
fn entries(dir: &Path, skipped: &mut usize) -> std::io::Result<Vec<Entry>> {
    let mut out = Vec::new();
    for entry in fs::read_dir(dir)? {
        let Ok(entry) = entry else {
            *skipped += 1;
            continue;
        };
        let Ok(name) = entry.file_name().into_string() else {
            *skipped += 1;
            continue;
        };
        if skip_name(&name) {
            continue;
        }
        let Ok(kind) = entry.file_type() else {
            *skipped += 1;
            continue;
        };
        if kind.is_symlink() || !(kind.is_dir() || kind.is_file()) {
            continue;
        }
        let len = if kind.is_file() {
            entry.metadata().map(|m| m.len()).unwrap_or(0)
        } else {
            0
        };
        out.push(Entry {
            name,
            path: entry.path(),
            is_dir: kind.is_dir(),
            len,
        });
    }
    out.sort_by_key(|e| e.name.clone());
    Ok(out)
}

fn track(file_name: String, format: String, len: u64) -> ScannedTrack {
    let leaf = file_name
        .rsplit('/')
        .next()
        .unwrap_or(&file_name)
        .to_string();
    let (track_number, title) = track_of(&leaf);
    ScannedTrack {
        file_name,
        track_number,
        title,
        format,
        size_bytes: i64::try_from(len).unwrap_or(i64::MAX),
    }
}

fn album(artist: &str, folder: &Entry, skipped: &mut usize) -> ScannedAlbum {
    let (name, year) = album_name_and_year(&folder.name);
    let mut tracks = Vec::new();
    let mut images = Vec::new();
    let mut other_files = 0;
    let listed = entries(&folder.path, skipped).unwrap_or_else(|_| {
        *skipped += 1;
        Vec::new()
    });
    for e in listed {
        if e.is_dir {
            // A disc folder: its audio belongs to the album.
            for f in entries(&e.path, skipped).unwrap_or_default() {
                match extension(&f.name) {
                    Some(ext) if !f.is_dir && AUDIO.contains(&ext.as_str()) => {
                        tracks.push(track(format!("{}/{}", e.name, f.name), ext, f.len));
                    }
                    _ if !f.is_dir => other_files += 1,
                    _ => {}
                }
            }
            continue;
        }
        match extension(&e.name) {
            Some(ext) if AUDIO.contains(&ext.as_str()) => tracks.push(track(e.name, ext, e.len)),
            Some(ext) if IMAGES.contains(&ext.as_str()) => images.push(e.name),
            _ => other_files += 1,
        }
    }
    // Disc folders after the album's own files, then by number, then by name.
    tracks.sort_by_key(|t| {
        (
            t.file_name.rsplit_once('/').map(|(d, _)| d.to_string()),
            t.track_number.unwrap_or(i32::MAX),
            t.file_name.clone(),
        )
    });
    ScannedAlbum {
        artist_folder: artist.to_string(),
        folder: folder.name.clone(),
        name_key: name_key(&name),
        name,
        year,
        tracks,
        cover_path: pick_cover(&images).map(|c| format!("{artist}/{}/{c}", folder.name)),
        other_files,
    }
}

/// Walk the music folder. Fails when the folder itself cannot be read; a sub-folder that
/// cannot be read is counted in `skipped`.
pub fn scan(root: &Path) -> Result<MusicTree, String> {
    let meta = fs::metadata(root).map_err(|e| format!("{} cannot be read: {e}", root.display()))?;
    if !meta.is_dir() {
        return Err(format!("{} is not a folder", root.display()));
    }
    let mut tree = MusicTree::default();
    let artists = entries(root, &mut tree.skipped)
        .map_err(|e| format!("{} cannot be listed: {e}", root.display()))?;
    for artist in artists.into_iter().filter(|a| a.is_dir) {
        let folders = match entries(&artist.path, &mut tree.skipped) {
            Ok(f) => f,
            Err(_) => {
                tree.skipped += 1;
                continue;
            }
        };
        let mut any = false;
        for folder in folders.iter().filter(|f| f.is_dir) {
            tree.albums
                .push(album(&artist.name, folder, &mut tree.skipped));
            any = true;
        }
        if any {
            tree.artists.push(artist.name);
        }
    }
    Ok(tree)
}

/// The file at `rel` under `root`, when it really is under it. A path with `..`, a root, a
/// drive or `.` is refused before the disk is asked; a path that resolves outside (through a
/// link) is refused after.
pub fn confine(root: &Path, rel: &str) -> Result<PathBuf, String> {
    let rel_path = Path::new(rel);
    if rel.trim().is_empty()
        || rel_path
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
    {
        return Err("the path leaves the folder".to_string());
    }
    let root = root
        .canonicalize()
        .map_err(|e| format!("the folder cannot be read: {e}"))?;
    let full = root
        .join(rel_path)
        .canonicalize()
        .map_err(|e| format!("the file cannot be read: {e}"))?;
    if !full.starts_with(&root) {
        return Err("the path leaves the folder".to_string());
    }
    Ok(full)
}

/// The content type of an image by its extension.
pub fn content_type(path: &str) -> Option<&'static str> {
    Some(match extension(path)?.as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "bmp" => "image/bmp",
        _ => return None,
    })
}

/// Why a cover cannot be served.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoverError {
    /// Missing, outside the folder, not an image, or unreadable.
    NotFound(String),
    TooLarge(u64),
}

/// A cover's bytes and content type: confined under the root, an image by extension, at
/// most [`MAX_COVER_BYTES`].
pub fn read_cover(root: &Path, rel: &str) -> Result<(Vec<u8>, &'static str), CoverError> {
    let kind = content_type(rel)
        .ok_or_else(|| CoverError::NotFound("the cover is not an image".to_string()))?;
    let path = confine(root, rel).map_err(CoverError::NotFound)?;
    let meta = fs::metadata(&path).map_err(|e| CoverError::NotFound(e.to_string()))?;
    if !meta.is_file() {
        return Err(CoverError::NotFound("the cover is not a file".to_string()));
    }
    if meta.len() > MAX_COVER_BYTES {
        return Err(CoverError::TooLarge(meta.len()));
    }
    let bytes = fs::read(&path).map_err(|e| CoverError::NotFound(e.to_string()))?;
    Ok((bytes, kind))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A folder of its own under the system's temporary folder, removed on drop.
    pub(crate) struct TempDir(pub PathBuf);

    impl TempDir {
        pub(crate) fn new(tag: &str) -> TempDir {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let dir = std::env::temp_dir().join(format!("mq-{tag}-{}-{nanos}", std::process::id()));
            fs::create_dir_all(&dir).unwrap();
            TempDir(dir)
        }

        pub(crate) fn file(&self, rel: &str, bytes: usize) {
            let path = self.0.join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, vec![b'x'; bytes]).unwrap();
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn folder_names_give_the_album_and_the_year() {
        assert_eq!(
            album_name_and_year("Riding With the King (2000)"),
            ("Riding With the King".to_string(), Some(2000))
        );
        assert_eq!(
            album_name_and_year("Baby Driver (Music from the Motion Picture) (0000)"),
            (
                "Baby Driver (Music from the Motion Picture)".to_string(),
                None
            )
        );
        assert_eq!(
            album_name_and_year("Listener Supported [Live]"),
            ("Listener Supported [Live]".to_string(), None)
        );
        assert_eq!(album_name_and_year("(1999)"), ("(1999)".to_string(), None));
        assert_eq!(
            name_key("Riding  with the King"),
            name_key("Riding With the King")
        );
        assert_eq!(sort_name("The Beatles"), "beatles");
    }

    #[test]
    fn file_names_give_the_track_number_and_title() {
        let t = |n: &str| track_of(n);
        assert_eq!(
            t("B.B. King - The Best Of B.B. King - 01 - Hummingbird.mp3"),
            (Some(1), Some("Hummingbird".to_string()))
        );
        assert_eq!(
            t("Adele - 21 - 03 - Turning Tables.flac"),
            (Some(3), Some("Turning Tables".to_string()))
        );
        assert_eq!(
            t("Taylor Swift - Red - 05 - 22.flac"),
            (Some(5), Some("22".to_string()))
        );
        assert_eq!(
            t("Jorja Smith - Black Panther The Album - 00 - I Am.mp3"),
            (None, Some("I Am".to_string()))
        );
        assert_eq!(t("07. Seven.flac"), (Some(7), Some("Seven".to_string())));
        assert_eq!(t("12 Twelve.mp3"), (Some(12), Some("Twelve".to_string())));
        assert_eq!(
            t("2-05 Disc Two.flac"),
            (Some(5), Some("Disc Two".to_string()))
        );
        assert_eq!(t("1999.mp3"), (None, Some("1999".to_string())));
        assert_eq!(t("Intro.mp3"), (None, Some("Intro".to_string())));
        assert_eq!(t("3rd Bass.mp3"), (None, Some("3rd Bass".to_string())));
        assert_eq!(extension("Track.FLAC").as_deref(), Some("flac"));
        assert_eq!(extension("noext"), None);
    }

    #[test]
    fn covers_are_picked_by_name() {
        let names = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            pick_cover(&names(&["back.jpg", "Cover.JPG", "folder.png"])).as_deref(),
            Some("Cover.JPG")
        );
        assert_eq!(
            pick_cover(&names(&["scan2.png", "scan1.png"])).as_deref(),
            Some("scan1.png")
        );
        assert_eq!(pick_cover(&[]), None);
        assert_eq!(content_type("a/b/cover.JPG"), Some("image/jpeg"));
        assert_eq!(content_type("a/b/notes.txt"), None);
    }

    #[test]
    fn a_walk_reads_artists_albums_tracks_and_covers() {
        let d = TempDir::new("music");
        // Two folders for one album; the names differ in more than case, so the test also
        // holds on a file system that ignores case.
        d.file("B.B. King/Riding  with the King (2000)/cover.jpg", 10);
        d.file(
            "B.B. King/Riding With the King (2000)/B.B. King - Riding With the King - 01 - Riding With the King.flac",
            300,
        );
        d.file(
            "B.B. King/Riding With the King (2000)/B.B. King - Riding With the King - 02 - Ten Long Years.flac",
            200,
        );
        d.file("B.B. King/Riding With the King (2000)/notes.nfo", 5);
        d.file("Various/Mixed Bag (0000)/01 - One.mp3", 30);
        d.file("Various/Mixed Bag (0000)/02 - Two.flac", 40);
        d.file("Various/Mixed Bag (0000)/CD2/01 - Three.mp3", 50);
        d.file("Various/Mixed Bag (0000)/folder.png", 4);
        d.file("Various/loose-track.mp3", 9);
        d.file("Empty Artist/.keep", 1);
        d.file(".hidden/Album (2001)/01 - x.mp3", 1);
        d.file("@eaDir/Album (2001)/01 - x.mp3", 1);

        let tree = scan(&d.0).unwrap();
        assert_eq!(tree.artists, vec!["B.B. King", "Various"]);
        assert_eq!(tree.albums.len(), 3);
        let find = |folder: &str| tree.albums.iter().find(|a| a.folder == folder).unwrap();
        let with_tracks = find("Riding With the King (2000)");
        assert_eq!(with_tracks.folder, "Riding With the King (2000)");
        assert_eq!(with_tracks.tracks.len(), 2);
        assert_eq!(with_tracks.tracks[0].track_number, Some(1));
        assert_eq!(
            with_tracks.tracks[1].title.as_deref(),
            Some("Ten Long Years")
        );
        assert_eq!(with_tracks.size_bytes(), 500);
        assert_eq!(with_tracks.cover_path, None);
        assert_eq!(with_tracks.other_files, 1);
        let cover_only = find("Riding  with the King (2000)");
        assert!(cover_only.tracks.is_empty());
        assert_eq!(
            cover_only.cover_path.as_deref(),
            Some("B.B. King/Riding  with the King (2000)/cover.jpg")
        );
        assert_eq!(
            cover_only.name_key, with_tracks.name_key,
            "the same album twice"
        );
        let mixed = find("Mixed Bag (0000)");
        assert_eq!(mixed.year, None);
        assert_eq!(mixed.formats(), vec!["flac", "mp3"]);
        assert_eq!(mixed.tracks.len(), 3);
        assert_eq!(mixed.tracks[2].file_name, "CD2/01 - Three.mp3");
        assert_eq!(
            mixed.cover_path.as_deref(),
            Some("Various/Mixed Bag (0000)/folder.png")
        );

        let (bytes, kind) = read_cover(&d.0, "Various/Mixed Bag (0000)/folder.png").unwrap();
        assert_eq!((bytes.len(), kind), (4, "image/png"));
        assert!(scan(&d.0.join("no-such-folder")).is_err());
    }

    #[test]
    fn a_cover_path_never_leaves_the_folder() {
        let d = TempDir::new("confine");
        d.file("Artist/Album (2000)/cover.jpg", 3);
        let outside = TempDir::new("outside");
        outside.file("secret.jpg", 3);
        assert!(confine(&d.0, "Artist/Album (2000)/cover.jpg").is_ok());
        for bad in [
            "../secret.jpg",
            "Artist/../../secret.jpg",
            "/etc/passwd",
            "./Artist/Album (2000)/cover.jpg",
            "",
        ] {
            assert!(confine(&d.0, bad).is_err(), "{bad} must be refused");
        }
        let escape = format!(
            "../{}/secret.jpg",
            outside.0.file_name().unwrap().to_string_lossy()
        );
        assert!(read_cover(&d.0, &escape).is_err());
        assert!(matches!(
            read_cover(&d.0, "Artist/Album (2000)/missing.jpg"),
            Err(CoverError::NotFound(_))
        ));
        d.file("Artist/Album (2000)/notes.txt", 3);
        assert!(read_cover(&d.0, "Artist/Album (2000)/notes.txt").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn a_link_out_of_the_folder_is_refused() {
        let d = TempDir::new("link");
        let outside = TempDir::new("link-outside");
        outside.file("secret.jpg", 3);
        fs::create_dir_all(d.0.join("Artist/Album (2000)")).unwrap();
        std::os::unix::fs::symlink(
            outside.0.join("secret.jpg"),
            d.0.join("Artist/Album (2000)/cover.jpg"),
        )
        .unwrap();
        assert!(read_cover(&d.0, "Artist/Album (2000)/cover.jpg").is_err());
        // The walk skips links, so the link is not taken for a cover either.
        let tree = scan(&d.0).unwrap();
        assert_eq!(tree.albums[0].cover_path, None);
    }
}
