//
// Copyright 2025 Christopher Atherton <the8lack8ox@pm.me>
//
// Permission is hereby granted, free of charge, to any person obtaining a copy
// of this software and associated documentation files (the “Software”), to
// deal in the Software without restriction, including without limitation the
// rights to use, copy, modify, merge, publish, distribute, sublicense, and/or
// sell copies of the Software, and to permit persons to whom the Software is
// furnished to do so, subject to the following conditions:
//
// The above copyright notice and this permission notice shall be included in
// all copies or substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED “AS IS”, WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
// IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
// FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL
// THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
// LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING
// FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS
// IN THE SOFTWARE.
//

use std::collections::{HashMap, VecDeque};
use std::env;
use std::fmt;
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::LazyLock;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[derive(Debug)]
enum ReflacError {
    InputTrackNotFound(usize),
    InvalidInputPath(PathBuf),
    InvalidTrackinfo(String),
    MissingInput(usize),
    NoFlacFilesFound(PathBuf),
    PathDoesNotExist(PathBuf),
    SubprocessError(&'static str),
    UnknownArchiveType(String),
}

impl fmt::Display for ReflacError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            ReflacError::InputTrackNotFound(track) => {
                write!(f, "Input file not found for track: {track}")
            }
            ReflacError::InvalidInputPath(path) => {
                write!(f, "Invalid input path: {}", path.display())
            }
            ReflacError::InvalidTrackinfo(line) => write!(f, "Invalid TRACKINFO line: {line}"),
            ReflacError::MissingInput(track) => write!(f, "Missing INPUT for track: {track}"),
            ReflacError::NoFlacFilesFound(path) => {
                write!(f, "No FLAC files found: {}", path.display())
            }
            ReflacError::PathDoesNotExist(path) => {
                write!(f, "Path does not exist: {}", path.display())
            }
            ReflacError::SubprocessError(cmd) => write!(f, "Failure executing: {cmd}"),
            ReflacError::UnknownArchiveType(ext) => write!(f, "Unknown archive type: {ext}"),
        }
    }
}

impl std::error::Error for ReflacError {}

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(prefix: &str) -> Self {
        let mut path = env::temp_dir().join(format!("{prefix}-{:08x}", rand::random::<u32>()));
        while path.exists() {
            path = env::temp_dir().join(format!("{prefix}-{:08x}", rand::random::<u32>()));
        }
        fs::create_dir(&path).expect("Could not create temporary directory");
        Self { path }
    }

    fn path(&self) -> &Path {
        self.path.as_path()
    }

    fn unique_subdir(&self) -> PathBuf {
        let mut sub_path = self.path.join(format!("{:08x}", rand::random::<u32>()));
        while sub_path.exists() {
            sub_path = self.path.join(format!("{:08x}", rand::random::<u32>()));
        }
        fs::create_dir(&sub_path).expect("Could not create unique temporary subdirectory");
        sub_path
    }

    fn unique_subfile(&self, ext: &str) -> (PathBuf, File) {
        let mut sub_path = self
            .path
            .join(format!("{:08x}{ext}", rand::random::<u32>()));
        while sub_path.exists() {
            sub_path = self
                .path
                .join(format!("{:08x}{ext}", rand::random::<u32>()));
        }
        (
            sub_path.clone(),
            File::create(sub_path).expect("Could not create unique temporary subfile"),
        )
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.path).expect("Could not remove temporary directory");
    }
}

#[derive(Clone)]
struct Tag {
    input: Option<String>,
    title: Option<String>,
    artist: Option<String>,
    lyricist: Option<String>,
    composer: Option<String>,
    arranger: Option<String>,
    album: Option<String>,
    track: Option<usize>,
    disc: Option<usize>,
    genre: Option<String>,
    date: Option<[u32; 3]>,
    label: Option<String>,
    comment: Option<String>,
    cover: Option<String>,
}

impl Tag {
    fn new() -> Self {
        Self {
            input: None,
            title: None,
            artist: None,
            lyricist: None,
            composer: None,
            arranger: None,
            album: None,
            track: None,
            disc: None,
            genre: None,
            date: None,
            label: None,
            comment: None,
            cover: None,
        }
    }

    fn output_path(&self, padding: usize) -> PathBuf {
        let mut ret = PathBuf::new();
        if let Some(disc) = self.disc {
            ret = ret.join(format!("Disc {disc}"));
        }
        if let Some(ref artist) = self.artist {
            if let Some(ref title) = self.title {
                ret.join(
                    format!(
                        "{:0fill$}. {artist} - {title}.flac",
                        self.track.unwrap(),
                        fill = padding
                    )
                    .replace("/", "_"),
                )
            } else {
                ret.join(
                    format!(
                        "{:0fill$}. {artist}.flac",
                        self.track.unwrap(),
                        fill = padding
                    )
                    .replace("/", "_"),
                )
            }
        } else if let Some(ref title) = self.title {
            ret.join(
                format!(
                    "{:0fill$}. {title}.flac",
                    self.track.unwrap(),
                    fill = padding
                )
                .replace("/", "_"),
            )
        } else {
            ret.join(
                format!("{:0fill$}.flac", self.track.unwrap(), fill = padding).replace("/", "_"),
            )
        }
    }
}

fn parse_trackinfo<P: AsRef<Path>>(path: P) -> Result<Vec<Tag>> {
    static INPUT_RE: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"INPUT(?:\[(\d+)\])?=(.*)").unwrap());
    static TITLE_RE: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"TITLE(?:\[(\d+)\])?=(.*)").unwrap());
    static ARTIST_RE: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"ARTIST(?:\[(\d+)\])?=(.*)").unwrap());
    static LYRICIST_RE: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"LYRICIST(?:\[(\d+)\])?=(.*)").unwrap());
    static COMPOSER_RE: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"COMPOSER(?:\[(\d+)\])?=(.*)").unwrap());
    static ARRANGER_RE: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"ARRANGER(?:\[(\d+)\])?=(.*)").unwrap());
    static ALBUM_RE: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"ALBUM(?:\[(\d+)\])?=(.*)").unwrap());
    static DISC_RE: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"DISC(?:\[(\d+)\])?=(\d+)").unwrap());
    static GENRE_RE: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"GENRE(?:\[(\d+)\])?=(.*)").unwrap());
    static DATE_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r"DATE(?:\[(\d+)\])?=(\d\d\d\d)-(\d\d)-(\d\d)").unwrap()
    });
    static LABEL_RE: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"LABEL(?:\[(\d+)\])?=(.*)").unwrap());
    static COMMENT_RE: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"COMMENT(?:\[(\d+)\])?=(.*)").unwrap());
    static COVER_RE: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"COVER(?:\[(\d+)\])?=(.*)").unwrap());

    let mut tags: Vec<Tag> = Vec::new();
    let mut global_tag = Tag::new();
    for line in BufReader::new(File::open(path)?)
        .lines()
        .map(|l| l.unwrap())
    {
        if let Some(caps) = INPUT_RE.captures(line.as_str()) {
            let input = caps[2].to_string();
            if let Some(mat) = caps.get(1) {
                let track = Some(mat.as_str().parse().unwrap());
                if let Some(tag) = tags.iter_mut().find(|t| t.track == track) {
                    tag.input = Some(input);
                } else {
                    let mut tag = global_tag.clone();
                    tag.track = track;
                    tag.input = Some(input);
                    tags.push(tag);
                }
            } else {
                global_tag.input = Some(input);
            }
        } else if let Some(caps) = TITLE_RE.captures(line.as_str()) {
            let trimmed = caps[2].trim().to_string();
            if trimmed != caps[2] {
                println!("WARNING: Line \"{}\" trimmed!", line);
            }
            if let Some(mat) = caps.get(1) {
                let track = Some(mat.as_str().parse().unwrap());
                if let Some(tag) = tags.iter_mut().find(|t| t.track == track) {
                    tag.title = Some(trimmed);
                } else {
                    let mut tag = global_tag.clone();
                    tag.track = track;
                    tag.title = Some(trimmed);
                    tags.push(tag);
                }
            } else {
                global_tag.title = Some(trimmed);
            }
        } else if let Some(caps) = ARTIST_RE.captures(line.as_str()) {
            let trimmed = caps[2].trim().to_string();
            if trimmed != caps[2] {
                println!("WARNING: Line \"{}\" trimmed!", line);
            }
            if let Some(mat) = caps.get(1) {
                let track = Some(mat.as_str().parse().unwrap());
                if let Some(tag) = tags.iter_mut().find(|t| t.track == track) {
                    tag.artist = Some(trimmed);
                } else {
                    let mut tag = global_tag.clone();
                    tag.track = track;
                    tag.artist = Some(trimmed);
                    tags.push(tag);
                }
            } else {
                global_tag.artist = Some(trimmed);
            }
        } else if let Some(caps) = LYRICIST_RE.captures(line.as_str()) {
            let trimmed = caps[2].trim().to_string();
            if trimmed != caps[2] {
                println!("WARNING: Line \"{}\" trimmed!", line);
            }
            if let Some(mat) = caps.get(1) {
                let track = Some(mat.as_str().parse().unwrap());
                if let Some(tag) = tags.iter_mut().find(|t| t.track == track) {
                    tag.lyricist = Some(trimmed);
                } else {
                    let mut tag = global_tag.clone();
                    tag.track = track;
                    tag.lyricist = Some(trimmed);
                    tags.push(tag);
                }
            } else {
                global_tag.lyricist = Some(trimmed);
            }
        } else if let Some(caps) = COMPOSER_RE.captures(line.as_str()) {
            let trimmed = caps[2].trim().to_string();
            if trimmed != caps[2] {
                println!("WARNING: Line \"{}\" trimmed!", line);
            }
            if let Some(mat) = caps.get(1) {
                let track = Some(mat.as_str().parse().unwrap());
                if let Some(tag) = tags.iter_mut().find(|t| t.track == track) {
                    tag.composer = Some(trimmed);
                } else {
                    let mut tag = global_tag.clone();
                    tag.track = track;
                    tag.composer = Some(trimmed);
                    tags.push(tag);
                }
            } else {
                global_tag.composer = Some(trimmed);
            }
        } else if let Some(caps) = ARRANGER_RE.captures(line.as_str()) {
            let trimmed = caps[2].trim().to_string();
            if trimmed != caps[2] {
                println!("WARNING: Line \"{}\" trimmed!", line);
            }
            if let Some(mat) = caps.get(1) {
                let track = Some(mat.as_str().parse().unwrap());
                if let Some(tag) = tags.iter_mut().find(|t| t.track == track) {
                    tag.arranger = Some(trimmed);
                } else {
                    let mut tag = global_tag.clone();
                    tag.track = track;
                    tag.arranger = Some(trimmed);
                    tags.push(tag);
                }
            } else {
                global_tag.arranger = Some(trimmed);
            }
        } else if let Some(caps) = ALBUM_RE.captures(line.as_str()) {
            let trimmed = caps[2].trim().to_string();
            if trimmed != caps[2] {
                println!("WARNING: Line \"{}\" trimmed!", line);
            }
            if let Some(mat) = caps.get(1) {
                let track = Some(mat.as_str().parse().unwrap());
                if let Some(tag) = tags.iter_mut().find(|t| t.track == track) {
                    tag.album = Some(trimmed);
                } else {
                    let mut tag = global_tag.clone();
                    tag.track = track;
                    tag.album = Some(trimmed);
                    tags.push(tag);
                }
            } else {
                global_tag.album = Some(trimmed);
            }
        } else if let Some(caps) = DISC_RE.captures(line.as_str()) {
            let disc = caps[2].parse().unwrap();
            if let Some(mat) = caps.get(1) {
                let track = Some(disc);
                if let Some(tag) = tags.iter_mut().find(|t| t.track == track) {
                    tag.disc = Some(disc);
                } else {
                    let mut tag = global_tag.clone();
                    tag.track = track;
                    tag.disc = Some(disc);
                    tags.push(tag);
                }
            } else {
                global_tag.disc = Some(caps[2].parse().unwrap());
            }
        } else if let Some(caps) = GENRE_RE.captures(line.as_str()) {
            let trimmed = caps[2].trim().to_string();
            if trimmed != caps[2] {
                println!("WARNING: Line \"{}\" trimmed!", line);
            }
            if let Some(mat) = caps.get(1) {
                let track = Some(mat.as_str().parse().unwrap());
                if let Some(tag) = tags.iter_mut().find(|t| t.track == track) {
                    tag.genre = Some(trimmed);
                } else {
                    let mut tag = global_tag.clone();
                    tag.track = track;
                    tag.genre = Some(trimmed);
                    tags.push(tag);
                }
            } else {
                global_tag.genre = Some(trimmed);
            }
        } else if let Some(caps) = DATE_RE.captures(line.as_str()) {
            let date = [caps[2].parse().unwrap(), caps[3].parse().unwrap(), caps[4].parse().unwrap()];
            if let Some(mat) = caps.get(1) {
                let track = Some(mat.as_str().parse().unwrap());
                if let Some(tag) = tags.iter_mut().find(|t| t.track == track) {
                    tag.date = Some(date);
                } else {
                    let mut tag = global_tag.clone();
                    tag.track = track;
                    tag.date = Some(date);
                    tags.push(tag);
                }
            } else {
                global_tag.date = Some(date);
            }
        } else if let Some(caps) = LABEL_RE.captures(line.as_str()) {
            let trimmed = caps[2].trim().to_string();
            if trimmed != caps[2] {
                println!("WARNING: Line \"{}\" trimmed!", line);
            }
            if let Some(mat) = caps.get(1) {
                let track = Some(mat.as_str().parse().unwrap());
                if let Some(tag) = tags.iter_mut().find(|t| t.track == track) {
                    tag.label = Some(trimmed);
                } else {
                    let mut tag = global_tag.clone();
                    tag.track = track;
                    tag.label = Some(trimmed);
                    tags.push(tag);
                }
            } else {
                global_tag.label = Some(trimmed);
            }
        } else if let Some(caps) = COMMENT_RE.captures(line.as_str()) {
            let trimmed = caps[2].trim().to_string();
            if trimmed != caps[2] {
                println!("WARNING: Line \"{}\" trimmed!", line);
            }
            if let Some(mat) = caps.get(1) {
                let track = Some(mat.as_str().parse().unwrap());
                if let Some(tag) = tags.iter_mut().find(|t| t.track == track) {
                    tag.comment = Some(trimmed);
                } else {
                    let mut tag = global_tag.clone();
                    tag.track = track;
                    tag.comment = Some(trimmed);
                    tags.push(tag);
                }
            } else {
                global_tag.comment = Some(trimmed);
            }
        } else if let Some(caps) = COVER_RE.captures(line.as_str()) {
            let path = caps[2].to_string();
            if let Some(mat) = caps.get(1) {
                let track = Some(mat.as_str().parse().unwrap());
                if let Some(tag) = tags.iter_mut().find(|t| t.track == track) {
                    tag.cover = Some(path);
                } else {
                    let mut tag = global_tag.clone();
                    tag.track = track;
                    tag.cover = Some(path);
                    tags.push(tag);
                }
            } else {
                global_tag.cover = Some(path);
            }
        } else if !line.is_empty() {
            return Err(ReflacError::InvalidTrackinfo(line).into());
        }
    }

    Ok(tags)
}

fn extract_archive<P: AsRef<Path>, Q: AsRef<Path>>(path: P, out_dir: Q) -> Result<()> {
    if let Some(ext) = path.as_ref().extension() {
        match ext.to_str().unwrap() {
            "zip" => {
                if !Command::new("unzip")
                    .arg(path.as_ref())
                    .arg("-d")
                    .arg(out_dir.as_ref())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()?
                    .success()
                {
                    return Err(ReflacError::SubprocessError("unzip").into());
                }
            }
            "rar" => {
                if !Command::new("unrar")
                    .arg("x")
                    .arg(path.as_ref())
                    .arg(out_dir.as_ref())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()?
                    .success()
                {
                    return Err(ReflacError::SubprocessError("unrar").into());
                }
            }
            "7z" => {
                if !Command::new("7za")
                    .arg("x")
                    .arg(format!("-o{}", out_dir.as_ref().to_str().unwrap()))
                    .arg(path.as_ref())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()?
                    .success()
                {
                    return Err(ReflacError::SubprocessError("7za").into());
                }
            }
            _ => {
                return Err(
                    ReflacError::UnknownArchiveType(ext.to_str().unwrap().to_string()).into(),
                );
            }
        }
    }
    Ok(())
}

fn get_input<P: AsRef<Path>>(path: P, tmp_dir: &TempDir) -> Result<PathBuf> {
    let mut progress = PathBuf::new();
    let mut pos = PathBuf::new();
    for p in path.as_ref() {
        progress = progress.join(p);
        pos = pos.join(p);
        if !pos.exists() {
            return Err(ReflacError::PathDoesNotExist(progress).into());
        }
        if pos.is_file() {
            if let Some(ext) = pos.extension() {
                let new_tree = tmp_dir.unique_subdir();
                if ["zip", "rar", "7z"].contains(&ext.to_str().unwrap()) {
                    extract_archive(pos, &new_tree)?;
                } else {
                    return Err(ReflacError::InvalidInputPath(progress).into());
                }
                let dir_contents: Vec<_> = fs::read_dir(&new_tree)?.collect();
                if dir_contents.len() == 1 {
                    pos = dir_contents[0].as_ref().unwrap().path();
                } else {
                    pos = new_tree;
                }
            } else {
                return Err(ReflacError::InvalidInputPath(progress).into());
            }
        }
    }
    Ok(pos)
}

fn search_input<P: AsRef<Path>>(path: P, tmp_dir: &TempDir) -> Result<PathBuf> {
    // Look for FLAC files
    for entry in fs::read_dir(&path)? {
        let entry = entry?;
        if entry.path().is_file() {
            if let Some(ext) = entry.path().extension() {
                if ext == "flac" {
                    return Ok(path.as_ref().to_path_buf());
                }
            }
        }
    }
    // Look in directories
    for entry in fs::read_dir(&path)? {
        let entry = entry?;
        if entry.path().is_dir() {
            let tree = search_input(entry.path(), tmp_dir);
            if tree.is_ok() {
                return tree;
            }
        }
    }
    // Look in archives
    for entry in fs::read_dir(&path)? {
        let entry = entry?;
        if entry.path().is_file() {
            if let Some(ext) = entry.path().extension() {
                if ["zip", "rar", "7z"].contains(&ext.to_str().unwrap()) {
                    let new_tree = tmp_dir.unique_subdir();
                    extract_archive(entry.path(), &new_tree)?;
                    let tree = search_input(new_tree, tmp_dir);
                    if tree.is_ok() {
                        return tree;
                    }
                }
            }
        }
    }
    // Nothing found
    Err(ReflacError::NoFlacFilesFound(path.as_ref().to_path_buf()).into())
}

fn get_track<P: AsRef<Path>>(track: usize, path: P) -> Result<PathBuf> {
    static TRACKFILE_RE: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r".*?(\d+).*\.flac").unwrap());
    for entry in path.as_ref().read_dir()? {
        let entry = entry?;
        if let Some(caps) = TRACKFILE_RE.captures(entry.file_name().to_str().unwrap()) {
            if caps[1].parse::<usize>().unwrap() == track {
                return Ok(entry.path());
            }
        }
    }
    Err(ReflacError::InputTrackNotFound(track).into())
}

fn get_cover<P: AsRef<Path>>(path: P, tmp_dir: &TempDir) -> Result<PathBuf> {
    if path.as_ref().exists() {
        if let Some(ext) = path.as_ref().extension() {
            if ext == "flac" {
                let (tmp_path, tmp_file) = tmp_dir.unique_subfile("");
                if !Command::new("metaflac")
                    .arg("--export-picture-to=-")
                    .arg(path.as_ref())
                    .stdout(tmp_file)
                    .stderr(Stdio::null())
                    .status()?
                    .success()
                {
                    eprintln!(
                        "ERROR! Failed to extract cover from {}!",
                        path.as_ref().display()
                    );
                    std::process::exit(1);
                }
                return Ok(tmp_path);
            }
        }
    } else {
        return Err(ReflacError::PathDoesNotExist(path.as_ref().to_path_buf()).into());
    }
    Ok(path.as_ref().to_path_buf())
}

fn get_album_name(tags: &Vec<Tag>) -> Option<&String> {
    let mut albums = HashMap::new();
    for tag in tags {
        if let Some(ref album) = tag.album {
            if let Some(cnt) = albums.get_mut(&album) {
                *cnt += 1;
            } else {
                albums.insert(album, 1);
            }
        }
    }
    let mut largest_cnt = 0;
    static EMPTY_STRING: String = String::new();
    let mut largest_album = &EMPTY_STRING;
    for (album, cnt) in albums {
        if cnt > largest_cnt {
            largest_cnt = cnt;
            largest_album = album;
        }
    }
    if largest_cnt > 0 {
        Some(largest_album)
    } else {
        None
    }
}

fn recompress<P: AsRef<Path>, Q: AsRef<Path>, R: AsRef<Path>>(
    in_path: P,
    out_path: Q,
    tag: &Tag,
    cover: Option<R>,
) -> Result<Child> {
    let dec_proc = Command::new("flac")
        .arg("--decode")
        .arg("--stdout")
        .arg(in_path.as_ref())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let mut args = vec![
        String::from("--best"),
        String::from("--exhaustive-model-search"),
        String::from("--qlp-coeff-precision-search"),
    ];
    if let Some(ref title) = tag.title {
        args.push(format!("--tag=TITLE={title}"));
    }
    if let Some(ref artist) = tag.artist {
        args.push(format!("--tag=ARTIST={artist}"));
    }
    if let Some(ref lyricist) = tag.lyricist {
        args.push(format!("--tag=LYRICIST={lyricist}"));
    }
    if let Some(ref composer) = tag.composer {
        args.push(format!("--tag=COMPOSER={composer}"));
    }
    if let Some(ref arranger) = tag.arranger {
        args.push(format!("--tag=ARRANGER={arranger}"));
    }
    if let Some(ref album) = tag.album {
        args.push(format!("--tag=ALBUM={album}"));
    }
    args.push(format!("--tag=TRACKNUMBER={}", tag.track.unwrap()));
    if let Some(disc) = tag.disc {
        args.push(format!("--tag=DISCNUMBER={disc}"));
    }
    if let Some(ref genre) = tag.genre {
        args.push(format!("--tag=GENRE={genre}"));
    }
    if let Some(ref date) = tag.date {
        args.push(format!(
            "--tag=DATE={:04}-{:02}-{:02}",
            date[0], date[1], date[2]
        ));
    }
    if let Some(ref label) = tag.label {
        args.push(format!("--tag=LABEL={label}"));
    }
    if let Some(ref comment) = tag.comment {
        args.push(format!("--tag=COMMENT={comment}"));
    }
    if let Some(path) = cover {
        args.push(format!("--picture={}", path.as_ref().to_str().unwrap()));
    }
    args.push(format!(
        "--output-name={}",
        out_path.as_ref().to_str().unwrap()
    ));
    args.push(String::from("-"));
    Ok(Command::new("flac")
        .args(args)
        .stdin(dec_proc.stdout.unwrap())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?)
}

fn add_replay_gain(paths: &Vec<PathBuf>) -> Result<()> {
    if !Command::new("metaflac")
        .arg("--add-replay-gain")
        .args(paths)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?
        .success()
    {
        todo!("Proper error handling");
    }
    Ok(())
}

fn run() -> Result<()> {
    // Assess command line
    if env::args().len() < 2 || env::args().len() > 3 {
        eprintln!(
            "USAGE: {} TRACKINFO [OUTPUT_DIR]",
            env::args().next().unwrap()
        );
        std::process::exit(1);
    }
    let trackinfo_path = PathBuf::from(env::args().nth(1).unwrap());
    let trackinfo_parent = trackinfo_path.parent().unwrap();
    let output_dir = if let Some(arg) = env::args().nth(2) {
        PathBuf::from(arg)
    } else if let Some(dirname) = trackinfo_path.parent() {
        dirname.to_path_buf()
    } else {
        eprintln!("ERROR: Could not evaluate TRACKINFO parent directory");
        std::process::exit(1);
    };
    if !trackinfo_path.exists() {
        eprintln!("ERROR: {} does not exist!", trackinfo_path.display());
        std::process::exit(1);
    }
    if !output_dir.exists() {
        eprintln!("ERROR: {} does not exist!", output_dir.display());
        std::process::exit(1);
    }
    if !output_dir.is_dir() {
        eprintln!("ERROR: {} is not a directory!", output_dir.display());
        std::process::exit(1);
    }

    // Parse trackinfo
    println!("Parsing track info file ...");
    let tags = parse_trackinfo(&trackinfo_path)?;

    // Work directory
    let work_dir = TempDir::new("reflac");

    // Resolve inputs
    let mut inputs_root: HashMap<&String, PathBuf> = HashMap::new();
    let mut inputs_flac: HashMap<&String, PathBuf> = HashMap::new();
    let mut input_map_roots: HashMap<usize, PathBuf> = HashMap::new();
    let mut input_map_flacs: HashMap<usize, PathBuf> = HashMap::new();
    for tag in &tags {
        let track = tag.track.unwrap();
        if let Some(ref input) = tag.input {
            if inputs_root.contains_key(input) {
                input_map_roots.insert(track, inputs_root[input].clone());
                input_map_flacs.insert(track, inputs_flac[input].clone());
            } else {
                println!("Opening input \"{input}\" ...");
                let root_path = get_input(trackinfo_parent.join(input), &work_dir)?;
                let flac_path = search_input(&root_path, &work_dir)?;
                input_map_roots.insert(track, root_path.clone());
                input_map_flacs.insert(track, flac_path.clone());
                inputs_root.insert(input, root_path);
                inputs_flac.insert(input, flac_path);
            }
        } else {
            todo!("Proper error handling");
        }
    }

    // Map input tracks
    println!("Mapping tracks ...");
    let mut source_map = HashMap::new();
    for tag in &tags {
        let track = tag.track.unwrap();
        let path = get_track(track, &input_map_flacs[&track])?;
        println!("  #{track} ← \"{}\"", path.file_name().unwrap().to_str().unwrap());
        source_map.insert(track, path);
    }

    // Locate covers
    let mut covers: HashMap<&String, PathBuf> = HashMap::new();
    let mut cover_map: HashMap<usize, PathBuf> = HashMap::new();
    for tag in &tags {
        let track = tag.track.unwrap();
        if let Some(ref cover) = tag.cover {
            if let Some(path) = covers.get(cover) {
                cover_map.insert(track, path.clone());
            } else {
                let path = get_cover(input_map_roots[&track].join(cover), &work_dir)?;
                cover_map.insert(track, path.clone());
                covers.insert(cover, path);
            }
        }
    }

    // Padding
    let padding = tags
        .iter()
        .map(|t| t.track.unwrap())
        .max()
        .unwrap()
        .to_string()
        .len();

    // Create album directory
    let album_path;
    let album_name = get_album_name(&tags);
    if let Some(album) = album_name {
        album_path = output_dir.join(album.replace("/", "_"));
    } else {
        todo!("Proper error handling");
    }
    fs::create_dir(&album_path)?;
    let mut discs = Vec::new();
    for tag in &tags {
        if let Some(disc) = tag.disc {
            if !discs.contains(&disc) {
                fs::create_dir(album_path.join(format!("Disc {disc}")))?;
                discs.push(disc);
            }
        }
    }

    // Recompress
    println!("Recompressing ...");
    let mut out_paths = Vec::new();
    let process_cnt = std::thread::available_parallelism()?.get();
    let mut process_next = VecDeque::from(tags);
    let mut process_working = VecDeque::with_capacity(process_cnt);
    for _ in 0..(std::cmp::min(process_next.len(), process_cnt) - 1) {
        let job = process_next.pop_front().unwrap();
        let out_path = album_path.join(job.output_path(padding));
        let track = job.track.unwrap();
        println!("  #{track} → \"{}\"", out_path.file_name().unwrap().to_str().unwrap());
        process_working.push_back(recompress(
            &source_map[&track],
            &out_path,
            &job,
            cover_map.get(&track),
        )?);
        out_paths.push(out_path);
    }
    while let Some(job) = process_next.pop_front() {
        let out_path = album_path.join(job.output_path(padding));
        let track = job.track.unwrap();
        println!("  #{track} → \"{}\"", out_path.file_name().unwrap().to_str().unwrap());
        process_working.push_back(recompress(
            &source_map[&track],
            &out_path,
            &job,
            cover_map.get(&track),
        )?);
        out_paths.push(out_path);

        if !process_working.pop_front().unwrap().wait()?.success() {
            todo!("Proper error handling");
        }
    }
    while let Some(ref mut job) = process_working.pop_front() {
        if !job.wait()?.success() {
            todo!("Proper error handling");
        }
    }

    // Add ReplayGain
    println!("Adding ReplayGain ...");
    add_replay_gain(&out_paths)?;

    Ok(())
}

fn main() {
    match run() {
        Ok(()) => (),
        Err(err) => {
            eprintln!("{err}");
            std::process::exit(1);
        }
    }
}
