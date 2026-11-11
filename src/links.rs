use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{self, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use serde_json::Value;

use crate::api;

pub const AUDIO_FORMAT: &str = "bestaudio/best";
pub const CACHE_ENV: &str = "YTM_TUI_LINKS";
const FRESH_FOR: Duration = Duration::from_hours(1);

pub struct LinkCache {
    dir: PathBuf,
}

impl LinkCache {
    pub fn create(dir: PathBuf) -> Result<Self> {
        fs::create_dir_all(&dir).with_context(|| format!("could not create {}", dir.display()))?;
        Ok(LinkCache { dir })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn prefetch(&self, id: &str) {
        let dir = self.dir.clone();
        let id = id.to_owned();
        thread::spawn(move || {
            let _ = resolve(&dir, &id);
        });
    }

    pub fn forget(&self, id: &str) {
        let _ = fs::remove_file(self.dir.join(format!("{id}.json")));
    }
}

impl Drop for LinkCache {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

pub fn run_as_ytdl(dir: &Path) -> Result<()> {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let Some(id) = video_id(&args) else {
        let err = Command::new("yt-dlp").args(&args).exec();
        return Err(err).context("could not run yt-dlp");
    };
    let info = resolve(dir, &id)?;
    wait_until_available(&info);
    io::stdout().write_all(&info)?;
    Ok(())
}

fn resolve(dir: &Path, id: &str) -> Result<Vec<u8>> {
    let lock = File::create(dir.join(format!("{id}.lock")))?;
    lock.lock()?;

    let path = dir.join(format!("{id}.json"));
    if is_fresh(&path) {
        return Ok(fs::read(&path)?);
    }

    let out = Command::new("yt-dlp")
        .args([
            "--no-warnings",
            "-J",
            "--no-playlist",
            "--format",
            AUDIO_FORMAT,
            "--",
        ])
        .arg(api::watch_url(id))
        .output()
        .context("could not run yt-dlp")?;
    if !out.status.success() {
        bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
    }

    let tmp = path.with_extension("tmp");
    fs::write(&tmp, &out.stdout)?;
    fs::rename(&tmp, &path)?;
    Ok(out.stdout)
}

fn is_fresh(path: &Path) -> bool {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|modified| modified.elapsed().ok())
        .is_some_and(|age| age < FRESH_FOR)
}

fn video_id(args: &[OsString]) -> Option<String> {
    let url = args.last()?.to_str()?;
    let id = url.strip_prefix("https://www.youtube.com/watch?v=")?;
    let valid = id.len() == 11
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    valid.then(|| id.to_owned())
}

fn wait_until_available(info: &[u8]) {
    let Ok(info) = serde_json::from_slice::<Value>(info) else {
        return;
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let ready_at = available_at(&info);
    if ready_at > now {
        thread::sleep(Duration::from_secs(ready_at - now));
    }
}

fn available_at(info: &Value) -> u64 {
    let formats = info["requested_formats"].as_array().into_iter().flatten();
    std::iter::once(info)
        .chain(formats)
        .filter_map(|f| f["available_at"].as_u64())
        .max()
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn args(url: &str) -> Vec<OsString> {
        ["--no-warnings", "-J", "--", url]
            .iter()
            .map(OsString::from)
            .collect()
    }

    #[test]
    fn takes_the_id_from_a_watch_url() {
        let id = video_id(&args("https://www.youtube.com/watch?v=tuoSS5o4CSI"));
        assert_eq!(id.as_deref(), Some("tuoSS5o4CSI"));
    }

    #[test]
    fn leaves_other_urls_to_yt_dlp() {
        assert_eq!(video_id(&args("https://example.com/song.mp3")), None);
        assert_eq!(
            video_id(&args("https://www.youtube.com/watch?v=tuoSS5o4CSI&list=RD")),
            None
        );
        assert_eq!(
            video_id(&args("https://www.youtube.com/watch?v=../../etc")),
            None
        );
    }

    #[test]
    fn waits_for_the_latest_format() {
        let info = json!({
            "available_at": 100,
            "requested_formats": [{ "available_at": 130 }, { "available_at": 110 }],
        });
        assert_eq!(available_at(&info), 130);
        assert_eq!(available_at(&json!({ "title": "no ads" })), 0);
    }
}
