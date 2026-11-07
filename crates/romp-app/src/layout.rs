use crate::romm::types::{RomDetail, RomFile};
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone)]
pub struct PlannedFile {
    pub file: RomFile,
    pub rel_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Launch {
    File(PathBuf),
    Playlist { name: String, discs: Vec<PathBuf> },
}

const DISC_ORDER: [&str; 8] = ["m3u", "cue", "gdi", "ccd", "cdi", "chd", "iso", "pbp"];

pub fn safe_component(name: &str) -> bool {
    let path = Path::new(name);
    path.components().count() == 1 && safe(path)
}

fn safe(rel: &Path) -> bool {
    !rel.as_os_str().is_empty() && rel.components().all(|c| matches!(c, Component::Normal(_)))
}

pub fn plan(rom: &RomDetail) -> Result<Vec<PlannedFile>, String> {
    let base = if rom.has_multiple_files {
        format!("{}/{}", rom.fs_path, rom.fs_name)
    } else {
        rom.fs_path.clone()
    };
    rom.files
        .iter()
        .map(|file| {
            let dir = file
                .file_path
                .strip_prefix(&base)
                .unwrap_or("")
                .trim_start_matches('/');
            let rel = Path::new(dir).join(&file.file_name);
            if file.file_name.is_empty() || !safe(&rel) {
                return Err(format!(
                    "unsafe file path: {}/{}",
                    file.file_path, file.file_name
                ));
            }
            Ok(PlannedFile {
                file: file.clone(),
                rel_path: rel,
            })
        })
        .collect()
}

fn ext(p: &Path) -> String {
    p.extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
}

const EXTRAS: [&str; 22] = [
    "txt", "nfo", "diz", "pdf", "doc", "docx", "md", "html", "htm", "url", "jpg", "jpeg", "png",
    "gif", "bmp", "xml", "sfv", "md5", "sha1", "srm", "sav", "log",
];

/// Readmes, covers and the like. A PICO-8 cart saved as a picture (`.p8.png`) is the game.
fn is_extra(p: &Path) -> bool {
    let pico8_cart = p.file_name().is_some_and(|n| {
        n.to_string_lossy()
            .to_ascii_lowercase()
            .ends_with(".p8.png")
    });
    EXTRAS.contains(&ext(p).as_str()) && !pico8_cart
}

pub fn launch_target(rom: &RomDetail, files: &[(PathBuf, u64)]) -> Option<Launch> {
    let depth = |p: &PathBuf| p.components().count();
    if rom.platform_slug == crate::rpcs3::PLATFORM {
        let eboot = files
            .iter()
            .map(|(p, _)| p)
            .filter(|p| {
                p.file_name()
                    .is_some_and(|n| n.eq_ignore_ascii_case("EBOOT.BIN"))
            })
            .min_by_key(|p| depth(p));
        if let Some(eboot) = eboot {
            return Some(Launch::File(eboot.clone()));
        }
    }
    let shallowest_of = |wanted: &str| {
        let matching: Vec<&PathBuf> = files
            .iter()
            .map(|(p, _)| p)
            .filter(|p| ext(p) == wanted)
            .collect();
        let shallowest = matching.iter().map(|p| depth(p)).min()?;
        let mut found: Vec<PathBuf> = matching
            .into_iter()
            .filter(|p| depth(p) == shallowest)
            .cloned()
            .collect();
        found.sort_by_cached_key(|p| (disc_number(p).unwrap_or(u32::MAX), p.clone()));
        Some(found)
    };
    // A disc beside the game's folder wins over one in a sub-folder, such as a translation.
    let disc_depth = files
        .iter()
        .map(|(p, _)| p)
        .filter(|p| DISC_ORDER.contains(&ext(p).as_str()))
        .map(&depth)
        .min();
    for wanted in DISC_ORDER {
        let Some(mut found) = shallowest_of(wanted) else {
            continue;
        };
        if found.first().map(depth) != disc_depth {
            continue;
        }
        if found.len() == 1 || wanted == "m3u" {
            return Some(Launch::File(found.swap_remove(0)));
        }
        return Some(playlist(&rom.fs_name, found));
    }
    if let Some(mut zips) = shallowest_of("zip").filter(|z| z.len() > 1) {
        let disks = crate::cores::core_for_platform(&rom.platform_slug)
            .is_some_and(|c| crate::cores::reads_zipped_disks(c.id));
        if disks {
            // PUAE picks the Amiga model from the name it's given, so the list carries the disks' tags,
            // such as "(AGA)", which the game's own name may not.
            let first = zips[0]
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned());
            return Some(playlist(first.as_deref().unwrap_or(&rom.fs_name), zips));
        }
        // Several zips are also an arcade game and its BIOS, which the fallback below handles.
        if zips.iter().any(|z| disc_number(z).is_some()) {
            return Some(Launch::File(zips.swap_remove(0)));
        }
    }
    files
        .iter()
        .filter(|(p, _)| !is_extra(p))
        .min_by(|(a, sa), (b, sb)| depth(a).cmp(&depth(b)).then(sb.cmp(sa)).then(a.cmp(b)))
        .map(|(p, _)| Launch::File(p.clone()))
}

/// Files RomM keeps beside a game that never start it.
const NOT_PLAYABLE: [&str; 6] = [
    "manual",
    "walkthrough",
    "screenshot",
    "soundtrack",
    "patch",
    "cheat",
];

/// Disc sheets, which start a disc made of the track files beside them.
const SHEETS: [&str; 4] = ["cue", "gdi", "ccd", "mds"];
const TRACKS: [&str; 4] = ["bin", "img", "raw", "sub"];

/// Whether `p` is a playlist naming a game's discs, whose entries live in other files.
pub fn is_playlist(p: &Path) -> bool {
    ext(p) == "m3u"
}

/// Whether `p` is part of a disc game: a disc, its sheets and tracks, or a playlist
/// naming them. Used to keep a server-side playlist playable without fetching a game's
/// unrelated files, which an `.m3u` can't name on its own.
pub fn is_disc_media(p: &Path) -> bool {
    let e = ext(p);
    e == "m3u"
        || DISC_ORDER.contains(&e.as_str())
        || SHEETS.contains(&e.as_str())
        || TRACKS.contains(&e.as_str())
}

/// The folders a playlist's discs live in: beside it, or in a folder named after it
/// (RomM keeps each translation set in `.Name`, after the playlist's own name).
fn set_folders(m3u: &Path) -> Vec<PathBuf> {
    let home = folder(m3u).to_path_buf();
    let stem = m3u
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    vec![home.clone(), home.join(format!(".{stem}")), home.join(stem)]
}

/// Whether `p` belongs to `m3u`'s set: the disc media beside it, or the files in its
/// set folders. Other playlists never do, so sets can't swallow each other.
fn in_set(m3u: &Path, p: &Path) -> bool {
    if p == m3u || is_playlist(p) || is_extra(p) {
        return false;
    }
    let folders = set_folders(m3u);
    let parent = folder(p);
    if parent == folders[0] {
        return is_disc_media(p);
    }
    folders[1..].iter().any(|d| *d == parent)
}

/// The discs a playlist starts: what belongs to its set. Empty when nothing claims it,
/// such as a playlist whose discs sit in an unrelated folder.
pub fn playlist_set(m3u: &Path, files: &[PlannedFile]) -> Vec<PathBuf> {
    if !is_playlist(m3u) {
        return Vec::new();
    }
    files
        .iter()
        .filter(|f| in_set(m3u, &f.rel_path))
        .map(|f| f.rel_path.clone())
        .collect()
}

/// What downloading a playlist needs: the playlist itself and its set, or all disc
/// media when it owns no set.
pub fn playlist_files(m3u: &Path, files: &[PlannedFile]) -> Vec<PathBuf> {
    let set = playlist_set(m3u, files);
    if set.is_empty() {
        return files
            .iter()
            .filter(|f| is_disc_media(&f.rel_path))
            .map(|f| f.rel_path.clone())
            .collect();
    }
    std::iter::once(m3u.to_path_buf()).chain(set).collect()
}

fn folder(p: &Path) -> &Path {
    p.parent().unwrap_or(Path::new(""))
}

/// Whether `p` is a track of a disc whose sheet is among `files`, so it doesn't start alone.
fn is_track(p: &Path, files: &[PlannedFile]) -> bool {
    TRACKS.contains(&ext(p).as_str())
        && files.iter().any(|f| {
            folder(&f.rel_path) == folder(p) && SHEETS.contains(&ext(&f.rel_path).as_str())
        })
}

/// The files a player can choose to start a multi-file game with: each playlist by
/// the name on its cover (without the extension) with its discs under it, then the
/// lone files, the one RomP starts by itself first; or nothing when RomP starts the
/// whole set itself: several discs with no playlist, a PS3 game.
///
/// RomM reports `has_multiple_files` as false when a folder holds one game file beside its
/// hacks or translations, so this goes by how many playable files there are instead.
pub fn file_choices(rom: &RomDetail, files: &[PlannedFile]) -> Vec<PathBuf> {
    if rom.platform_slug == crate::rpcs3::PLATFORM {
        return Vec::new();
    }
    let sized: Vec<(PathBuf, u64)> = files
        .iter()
        .map(|f| (f.rel_path.clone(), f.file.file_size_bytes.max(0) as u64))
        .collect();
    let Some(Launch::File(automatic)) = launch_target(rom, &sized) else {
        return Vec::new();
    };
    let mut pool: Vec<PathBuf> = files
        .iter()
        .filter(|f| {
            f.file
                .category
                .as_deref()
                .is_none_or(|c| !NOT_PLAYABLE.contains(&c))
        })
        .map(|f| f.rel_path.clone())
        .filter(|p| !is_extra(p) && ext(p) != "m3u" && !is_track(p, files))
        .collect();
    // Playlists that own discs, the automatic set first, then by name. A playlist with
    // no set stays hidden: its discs show as lone files instead.
    let mut lists: Vec<PathBuf> = files
        .iter()
        .map(|f| &f.rel_path)
        .filter(|p| is_playlist(p) && !playlist_set(p, files).is_empty())
        .cloned()
        .collect();
    lists.sort_by_cached_key(|m| (m != &automatic, m.to_string_lossy().to_lowercase()));
    let mut grouped: Vec<PathBuf> = Vec::new();
    for m3u in &lists {
        let mut discs = playlist_set(m3u, files);
        discs.sort_by_cached_key(|p| p.to_string_lossy().to_lowercase());
        grouped.extend(discs);
    }
    // The automatic choice leads, then each set with its discs, then the lone files.
    let mut ordered = Vec::new();
    if !lists.contains(&automatic) {
        ordered.push(automatic.clone());
    }
    for m3u in &lists {
        ordered.push(m3u.clone());
        let mut discs = playlist_set(m3u, files);
        discs.sort_by_cached_key(|p| p.to_string_lossy().to_lowercase());
        ordered.extend(discs);
    }
    pool.sort_by_cached_key(|p| p.to_string_lossy().to_lowercase());
    for p in pool {
        if p != automatic && !grouped.contains(&p) {
            ordered.push(p);
        }
    }
    if ordered.len() < 2 {
        return Vec::new();
    }
    ordered
}

/// How the file menu shows its entries: playlists by their name, their discs offset
/// under them, lone files as they are.
pub fn file_labels(choices: &[PathBuf]) -> Vec<String> {
    fn stem(p: &Path) -> String {
        p.file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
    }
    fn name(p: &Path) -> String {
        p.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    }
    let mut labels = Vec::with_capacity(choices.len());
    let mut set: Option<PathBuf> = None;
    for p in choices {
        if is_playlist(p) {
            labels.push(stem(p));
            set = Some(p.clone());
        } else {
            let member = set.as_deref().is_some_and(|m| in_set(m, p));
            if !member {
                set = None;
            }
            labels.push(format!("{}{}", if member { "  " } else { "" }, name(p)));
        }
    }
    labels
}

/// The files that starting with `pick` needs: it, and the tracks beside it when it's a sheet.
pub fn files_for(pick: &Path, files: &[PlannedFile]) -> Vec<PathBuf> {
    let sheet = SHEETS.contains(&ext(pick).as_str());
    files
        .iter()
        .map(|f| &f.rel_path)
        .filter(|p| {
            *p == pick || (sheet && folder(p) == folder(pick) && TRACKS.contains(&ext(p).as_str()))
        })
        .cloned()
        .collect()
}

fn playlist(stem: &str, discs: Vec<PathBuf>) -> Launch {
    let name = format!("{stem}.m3u");
    let name = if safe_component(&name) {
        name
    } else {
        "game.m3u".to_string()
    };
    Launch::Playlist { name, discs }
}

/// The disc a file's name says it is, as RomM reads it: "(Disc 2)", "(CD 2)", "(Disk 1 of 2)",
/// "(Disc 2A)" or "(Disc B)", where B is 2.
fn disc_number(p: &Path) -> Option<u32> {
    let name = p.file_name()?.to_string_lossy().to_ascii_lowercase();
    name.split('(')
        .filter_map(|group| group.split_once(')'))
        .find_map(|(tag, _)| {
            let rest = ["disque", "disc", "disk", "cd"]
                .iter()
                .find_map(|word| tag.strip_prefix(word))?;
            let mut words: Vec<&str> = rest.split_whitespace().collect();
            if let [.., of, total] = words[..] {
                if of == "of" && total.chars().all(|c| c.is_ascii_digit()) {
                    words.truncate(words.len() - 2);
                }
            }
            let [word] = words[..] else { return None };
            let digits = word.trim_end_matches(|c: char| c.is_ascii_lowercase());
            if !digits.is_empty() && word.len() - digits.len() <= 1 {
                return digits.parse().ok();
            }
            let spaced = rest.starts_with(char::is_whitespace);
            match word.as_bytes() {
                [letter @ b'a'..=b'z'] if spaced => Some(u32::from(letter - b'a') + 1),
                _ => None,
            }
        })
}

pub fn m3u(discs: &[PathBuf]) -> String {
    discs.iter().map(|d| format!("{}\n", d.display())).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(id: i64, path: &str, name: &str) -> RomFile {
        RomFile {
            id,
            file_name: name.into(),
            file_path: path.into(),
            file_size_bytes: 1,
            sha1_hash: None,
            category: None,
        }
    }

    fn rom(multi: bool, files: Vec<RomFile>) -> RomDetail {
        RomDetail {
            id: 1,
            platform_slug: "psx".into(),
            fs_name: "Arc III".into(),
            fs_path: "roms/psx".into(),
            has_multiple_files: multi,
            files,
            ra_hash: None,
            ra_id: None,
            merged_ra_metadata: None,
        }
    }

    fn sized(paths: &[PathBuf]) -> Vec<(PathBuf, u64)> {
        paths.iter().map(|p| (p.clone(), 1)).collect()
    }

    fn rels(r: &RomDetail) -> Vec<PathBuf> {
        plan(r).unwrap().into_iter().map(|p| p.rel_path).collect()
    }

    fn choices(files: Vec<RomFile>) -> Vec<PathBuf> {
        let mut r = rom(true, files);
        r.fs_path = "roms/snes".into();
        r.fs_name = "Super Metroid".into();
        r.platform_slug = "snes".into();
        let planned = plan(&r).unwrap();
        file_choices(&r, &planned)
    }

    #[test]
    fn a_folder_of_hacks_offers_each_one() {
        let dir = "roms/snes/Super Metroid";
        let mut manual = file(4, dir, "Manual.pdf");
        manual.category = Some("manual".into());
        let mut patch = file(5, &format!("{dir}/patch"), "Fix.ips");
        patch.category = Some("patch".into());
        assert_eq!(
            choices(vec![
                file(1, dir, "Super Metroid (Japan, USA).zip"),
                file(2, &format!("{dir}/hack"), "Super Metroid - Advent.smc"),
                file(3, dir, "readme.txt"),
                manual,
                patch,
                file(6, dir, "Hyper Metroid.smc"),
            ]),
            [
                PathBuf::from("Hyper Metroid.smc"),
                PathBuf::from("hack/Super Metroid - Advent.smc"),
                PathBuf::from("Super Metroid (Japan, USA).zip"),
            ]
        );
    }

    #[test]
    fn single_flag_games_with_hacks_offer_each_one() {
        // RomM reports `has_multiple_files` as false when a folder holds one game file
        // beside its hacks (see RomP issue #39: Super Metroid with 1 game + 34 hacks).
        let dir = "roms/snes/Super Metroid (Japan, USA)";
        let mut r = rom(
            false,
            vec![
                file(1, dir, "Super Metroid (Japan, USA).zip"),
                file(2, &format!("{dir}/hack"), "Hyper Metroid.smc"),
                file(3, &format!("{dir}/hack"), "Super Metroid - Advent.smc"),
            ],
        );
        r.fs_path = "roms/snes".into();
        r.fs_name = "Super Metroid (Japan, USA)".into();
        r.platform_slug = "snes".into();
        let planned = plan(&r).unwrap();
        let got = file_choices(&r, &planned);
        assert_eq!(got.len(), 3);
        assert!(got
            .iter()
            .any(|p| p.ends_with("Super Metroid (Japan, USA).zip")));
        assert!(got.iter().any(|p| p.ends_with("hack/Hyper Metroid.smc")));
    }

    #[test]
    fn a_disc_offers_its_translations() {
        let dir = "roms/snes/Super Metroid";
        let translations = format!("{dir}/translation");
        assert_eq!(
            choices(vec![
                file(1, dir, "Tales (Japan).chd"),
                file(2, &translations, "Tales (T-En).chd"),
                file(3, &translations, "Tales (T-En 2).cue"),
                file(4, &translations, "Tales (T-En 2) (Track 1).bin"),
            ]),
            [
                PathBuf::from("Tales (Japan).chd"),
                PathBuf::from("translation/Tales (T-En 2).cue"),
                PathBuf::from("translation/Tales (T-En).chd"),
            ]
        );
    }

    #[test]
    fn a_sheet_comes_down_with_its_tracks() {
        let dir = "roms/snes/Super Metroid";
        let translations = format!("{dir}/translation");
        let mut r = rom(
            true,
            vec![
                file(1, dir, "Tales (Japan).chd"),
                file(2, &translations, "Tales (T-En).cue"),
                file(3, &translations, "Tales (T-En) (Track 1).bin"),
                file(4, &translations, "Tales (T-En) (Track 2).bin"),
            ],
        );
        r.fs_path = "roms/snes".into();
        r.fs_name = "Super Metroid".into();
        let planned = plan(&r).unwrap();
        assert_eq!(
            files_for(Path::new("translation/Tales (T-En).cue"), &planned).len(),
            3
        );
        assert_eq!(
            files_for(Path::new("Tales (Japan).chd"), &planned),
            [PathBuf::from("Tales (Japan).chd")]
        );
    }

    #[test]
    fn discs_and_lone_games_offer_no_choice() {
        let dir = "roms/snes/Super Metroid";
        assert!(choices(vec![
            file(1, dir, "Game (Disc 1).chd"),
            file(2, dir, "Game (Disc 2).chd"),
        ])
        .is_empty());
        assert!(choices(vec![file(1, dir, "Game.cue"), file(2, dir, "Game.bin")]).is_empty());
        assert!(choices(vec![file(1, dir, "Game.smc"), file(2, dir, "readme.txt")]).is_empty());
        let r = rom(false, vec![file(1, "roms/snes", "Game.smc")]);
        assert!(file_choices(&r, &plan(&r).unwrap()).is_empty());
    }

    #[test]
    fn a_disc_beside_the_game_starts_before_one_in_a_sub_folder() {
        let r = rom(true, Vec::new());
        let files = sized(&[
            PathBuf::from("Tales (Japan).chd"),
            PathBuf::from("translation/Tales (T-En).cue"),
            PathBuf::from("translation/Tales (T-En) (Track 1).bin"),
        ]);
        assert_eq!(
            launch_target(&r, &files),
            Some(Launch::File(PathBuf::from("Tales (Japan).chd")))
        );
    }

    #[test]
    fn single_file_goes_to_root() {
        assert_eq!(
            rels(&rom(false, vec![file(1, "roms/psx", "Game.chd")])),
            [PathBuf::from("Game.chd")]
        );
    }

    #[test]
    fn multi_file_keeps_sub_folders() {
        let r = rom(
            true,
            vec![
                file(1, "roms/psx/Arc III", "Disc 1.chd"),
                file(2, "roms/psx/Arc III/extra", "readme.txt"),
            ],
        );
        assert_eq!(
            rels(&r),
            [
                PathBuf::from("Disc 1.chd"),
                PathBuf::from("extra/readme.txt")
            ]
        );
    }

    #[test]
    fn plan_rejects_escaping_paths() {
        assert!(plan(&rom(false, vec![file(1, "roms/psx", "../evil")])).is_err());
        assert!(plan(&rom(true, vec![file(1, "roms/psx/Arc III/../../..", "x")])).is_err());
        assert!(plan(&rom(false, vec![file(1, "roms/psx", "/etc/passwd")])).is_err());
        assert!(plan(&rom(false, vec![file(1, "roms/psx", "")])).is_err());
    }

    #[test]
    fn ps3_game_folders_launch_their_eboot() {
        let mut r = rom(true, vec![]);
        r.platform_slug = "ps3".into();
        let files = [
            (PathBuf::from("PS3_DISC.SFB"), 1),
            (PathBuf::from("PS3_GAME/USRDIR/data/level.iso"), 900),
            (PathBuf::from("PS3_GAME/USRDIR/data/EBOOT.BIN"), 5),
            (PathBuf::from("PS3_GAME/USRDIR/EBOOT.BIN"), 10),
        ];
        assert_eq!(
            launch_target(&r, &files),
            Some(Launch::File(PathBuf::from("PS3_GAME/USRDIR/EBOOT.BIN")))
        );
        let iso = [
            (PathBuf::from("Game.iso"), 1),
            (PathBuf::from("Game.dkey"), 1),
        ];
        assert_eq!(
            launch_target(&r, &iso),
            Some(Launch::File(PathBuf::from("Game.iso")))
        );
    }

    #[test]
    fn two_discs_make_a_playlist() {
        let r = rom(true, vec![]);
        let files = [
            PathBuf::from("Arc (Disc 2).chd"),
            PathBuf::from("Arc (Disc 1).chd"),
        ];
        match launch_target(&r, &sized(&files)).unwrap() {
            Launch::Playlist { name, discs } => {
                assert_eq!(name, "Arc III.m3u");
                assert_eq!(
                    discs,
                    [
                        PathBuf::from("Arc (Disc 1).chd"),
                        PathBuf::from("Arc (Disc 2).chd")
                    ]
                );
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(
            m3u(&[PathBuf::from("a.chd"), PathBuf::from("b.chd")]),
            "a.chd\nb.chd\n"
        );
    }

    #[test]
    fn discs_play_in_the_order_their_names_number_them() {
        let r = rom(true, vec![]);
        let files = sized(&[
            PathBuf::from("Game (Disc 10).chd"),
            PathBuf::from("Game (Bonus).chd"),
            PathBuf::from("Game (Disc 2).chd"),
            PathBuf::from("Game (Disc 1).chd"),
        ]);
        match launch_target(&r, &files).unwrap() {
            Launch::Playlist { discs, .. } => assert_eq!(
                discs,
                [
                    "Game (Disc 1).chd",
                    "Game (Disc 2).chd",
                    "Game (Disc 10).chd",
                    "Game (Bonus).chd"
                ]
                .map(PathBuf::from)
            ),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn zipped_disks_make_a_playlist_for_an_emulator_that_opens_zips() {
        let mut r = rom(true, vec![]);
        r.platform_slug = "amiga".into();
        let files = vec![
            (PathBuf::from("Soccer Kid_Disk1.zip"), 767_602),
            (PathBuf::from("Soccer Kid_Disk2.zip"), 794_874),
            (PathBuf::from("Soccer Kid_Disk0.zip"), 703_522),
        ];
        match launch_target(&r, &files).unwrap() {
            Launch::Playlist { name, discs } => {
                assert_eq!(name, "Soccer Kid_Disk0.m3u");
                assert_eq!(
                    discs,
                    [
                        "Soccer Kid_Disk0.zip",
                        "Soccer Kid_Disk1.zip",
                        "Soccer Kid_Disk2.zip"
                    ]
                    .map(PathBuf::from)
                );
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn zipped_discs_start_from_the_first_for_other_emulators() {
        let r = rom(true, vec![]);
        let files = vec![
            (PathBuf::from("Game (Disc 2).zip"), 900),
            (PathBuf::from("Game (Disc 1).zip"), 800),
        ];
        assert_eq!(
            launch_target(&r, &files),
            Some(Launch::File(PathBuf::from("Game (Disc 1).zip")))
        );
    }

    #[test]
    fn an_arcade_game_beside_its_bios_still_starts_the_game() {
        let mut r = rom(true, vec![]);
        r.platform_slug = "arcade".into();
        let files = vec![
            (PathBuf::from("neogeo.zip"), 1_000),
            (PathBuf::from("mslug.zip"), 9_000),
        ];
        assert_eq!(
            launch_target(&r, &files),
            Some(Launch::File(PathBuf::from("mslug.zip")))
        );
    }

    #[test]
    fn disc_numbers_are_read_the_ways_rom_sets_write_them() {
        let number = |name: &str| disc_number(Path::new(name));
        assert_eq!(number("Game (Disc 2).cue"), Some(2));
        assert_eq!(number("Game (CD 3).chd"), Some(3));
        assert_eq!(number("Game (Disk 1 of 2).adf"), Some(1));
        assert_eq!(number("Game (disque 2).cue"), Some(2));
        assert_eq!(number("Game (Disc 2A).cue"), Some(2));
        assert_eq!(number("Game (Disc B).cue"), Some(2));
        assert_eq!(number("Game (CDi).chd"), None);
        assert_eq!(number("Game (Discovery).chd"), None);
        assert_eq!(number("Game.chd"), None);
    }

    #[test]
    fn playlists_keep_their_disc_media_but_not_extras() {
        let media = |name: &str| is_disc_media(Path::new(name));
        assert!(is_playlist(Path::new("Game.m3u")));
        assert!(!is_playlist(Path::new("Game.cue")));
        assert!(media("Game.m3u"));
        assert!(media("Arc (Disc 1).chd"));
        assert!(media("CD/Game.cue"));
        assert!(media("CD/Game (Track 1).bin"));
        assert!(!media("01 Tune.spc"));
        assert!(!media("7.pdf"));
        assert!(!media("Hyper.smc"));
    }

    #[test]
    fn playlists_offer_their_sets_with_discs_under_them() {
        let dir = "roms/psx/Game";
        let mut r = rom(
            true,
            vec![
                file(1, dir, "Game.m3u"),
                file(2, &format!("{dir}/.Game"), "Game (Disc 1).chd"),
                file(3, &format!("{dir}/.Game"), "Game (Disc 2).chd"),
                file(4, &format!("{dir}/translation"), "T.m3u"),
                file(5, &format!("{dir}/translation/.T"), "T (Disc 1).chd"),
                file(6, &format!("{dir}/manual"), "7.pdf"),
            ],
        );
        r.fs_path = "roms/psx".into();
        r.fs_name = "Game".into();
        r.platform_slug = "psx".into();
        let planned = plan(&r).unwrap();
        let got = file_choices(&r, &planned);
        assert_eq!(
            got,
            [
                "Game.m3u",
                ".Game/Game (Disc 1).chd",
                ".Game/Game (Disc 2).chd",
                "translation/T.m3u",
                "translation/.T/T (Disc 1).chd",
            ]
            .map(PathBuf::from)
        );
        assert_eq!(
            file_labels(&got),
            [
                "Game",
                "  Game (Disc 1).chd",
                "  Game (Disc 2).chd",
                "T",
                "  T (Disc 1).chd",
            ]
        );
    }

    #[test]
    fn lone_files_stay_flat() {
        let dir = "roms/snes/Metroid";
        let mut r = rom(
            false,
            vec![
                file(1, dir, "Metroid.zip"),
                file(2, &format!("{dir}/hack"), "Hyper.smc"),
            ],
        );
        r.fs_path = "roms/snes".into();
        r.fs_name = "Metroid".into();
        r.platform_slug = "snes".into();
        let planned = plan(&r).unwrap();
        let got = file_choices(&r, &planned);
        assert_eq!(got.len(), 2);
        assert_eq!(
            file_labels(&got),
            ["Metroid.zip".to_string(), "Hyper.smc".to_string()]
        );
    }

    #[test]
    fn cue_wins_over_bin_and_carts_use_their_file() {
        let r = rom(true, vec![]);
        let files = [
            PathBuf::from("Game (Track 1).bin"),
            PathBuf::from("Game.cue"),
            PathBuf::from("Game (Track 2).bin"),
        ];
        assert!(
            matches!(launch_target(&r, &sized(&files)), Some(Launch::File(p)) if p == Path::new("Game.cue"))
        );
        assert!(
            matches!(launch_target(&r, &sized(&[PathBuf::from("Zelda.sfc")])), Some(Launch::File(p)) if p == Path::new("Zelda.sfc"))
        );
        assert!(launch_target(&r, &[]).is_none());
    }

    #[test]
    fn playlist_name_never_leaves_the_game_folder() {
        let mut r = rom(true, vec![]);
        r.fs_name = "../../evil".into();
        let files = [PathBuf::from("a.chd"), PathBuf::from("b.chd")];
        match launch_target(&r, &sized(&files)).unwrap() {
            Launch::Playlist { name, .. } => assert_eq!(name, "game.m3u"),
            other => panic!("{other:?}"),
        }
        assert!(safe_component("psx"));
        assert!(!safe_component(".."));
        assert!(!safe_component("a/b"));
        assert!(!safe_component(""));
    }

    #[test]
    fn discs_in_a_sub_folder_are_found() {
        let r = rom(true, vec![]);
        let files = sized(&[
            PathBuf::from("readme.txt"),
            PathBuf::from("CD/Game.cue"),
            PathBuf::from("CD/Game.bin"),
        ]);
        assert_eq!(
            launch_target(&r, &files),
            Some(Launch::File(PathBuf::from("CD/Game.cue")))
        );
    }

    #[test]
    fn fallback_skips_extras_and_prefers_the_largest_file() {
        let r = rom(true, vec![]);
        let files = vec![
            (PathBuf::from("a-manual.pdf"), 900),
            (PathBuf::from("b-readme.txt"), 5),
            (PathBuf::from("game.bin"), 100),
            (PathBuf::from("patch.ips"), 10),
        ];
        assert_eq!(
            launch_target(&r, &files),
            Some(Launch::File(PathBuf::from("game.bin")))
        );
        assert_eq!(launch_target(&r, &[(PathBuf::from("notes.txt"), 1)]), None);
    }

    #[test]
    fn a_pico8_cart_saved_as_a_picture_is_the_game() {
        let r = rom(false, vec![]);
        let cart = PathBuf::from("Across The River.p8.png");
        assert_eq!(
            launch_target(&r, &[(cart.clone(), 32_224)]),
            Some(Launch::File(cart.clone()))
        );
        let with_cover = [(PathBuf::from("cover.png"), 90_000), (cart.clone(), 32_224)];
        assert_eq!(launch_target(&r, &with_cover), Some(Launch::File(cart)));
    }
}
