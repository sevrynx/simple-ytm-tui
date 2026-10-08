use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::Sender;
use std::thread;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PlayerEvent {
    TimePos(Option<f64>),
    Duration(Option<f64>),
    Paused(bool),
    Volume(f64),
    PlaylistPos(Option<usize>),
    PlaybackError,
    Exited,
}

const OBSERVED: [&str; 5] = ["time-pos", "duration", "pause", "volume", "playlist-pos"];

pub struct Player {
    child: Child,
    sock: UnixStream,
    sock_path: PathBuf,
}

impl Player {
    pub fn spawn<T>(tx: Sender<T>) -> Result<Self>
    where
        T: From<PlayerEvent> + Send + 'static,
    {
        let sock_path = socket_path();
        let _ = fs::remove_file(&sock_path);

        let mut child = spawn_mpv(&sock_path)?;
        let Some(sock) = connect_with_retry(&sock_path) else {
            let _ = child.kill();
            let _ = child.wait();
            bail!("mpv started but its IPC socket never appeared");
        };

        let reader = sock.try_clone()?;
        thread::spawn(move || forward_events(reader, &tx));

        let mut player = Player {
            child,
            sock,
            sock_path,
        };
        for (id, name) in OBSERVED.iter().enumerate() {
            player.observe(id + 1, name);
        }
        Ok(player)
    }

    pub fn play(&mut self, url: &str) {
        self.command(json!(["loadfile", url, "replace"]));
    }

    pub fn insert_at(&mut self, index: usize, url: &str) {
        self.command(json!(["loadfile", url, "insert-at", index]));
    }

    pub fn append(&mut self, url: &str) {
        self.command(json!(["loadfile", url, "append-play"]));
    }

    pub fn remove(&mut self, index: usize) {
        self.command(json!(["playlist-remove", index]));
    }

    pub fn jump_to(&mut self, index: usize) {
        self.command(json!(["set_property", "playlist-pos", index]));
    }

    pub fn next(&mut self) {
        self.command(json!(["playlist-next"]));
    }

    pub fn previous(&mut self) {
        self.command(json!(["playlist-prev"]));
    }

    pub fn toggle_pause(&mut self) {
        self.command(json!(["cycle", "pause"]));
    }

    pub fn seek_by(&mut self, secs: f64) {
        self.command(json!(["seek", secs, "relative"]));
    }

    pub fn restart(&mut self) {
        self.command(json!(["seek", 0, "absolute"]));
    }

    pub fn change_volume(&mut self, delta: f64) {
        self.command(json!(["add", "volume", delta]));
    }

    fn observe(&mut self, id: usize, property: &str) {
        self.command(json!(["observe_property", id, property]));
    }

    fn quit(&mut self) {
        self.command(json!(["quit"]));
    }

    fn command(&mut self, args: Value) {
        let mut line = json!({ "command": args }).to_string();
        line.push('\n');
        let _ = self.sock.write_all(line.as_bytes());
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.quit();
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = fs::remove_file(&self.sock_path);
    }
}

fn socket_path() -> PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    dir.join(format!("ytm-tui-{}.sock", std::process::id()))
}

fn spawn_mpv(sock_path: &Path) -> Result<Child> {
    let mut cmd = Command::new("mpv");
    cmd.args([
        "--idle=yes",
        "--no-video",
        "--no-terminal",
        "--force-window=no",
        "--ytdl-format=bestaudio/best",
    ])
    .arg(format!("--input-ipc-server={}", sock_path.display()))
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::null());
    // SAFETY: prctl is async-signal-safe and touches no shared state.
    unsafe {
        cmd.pre_exec(|| {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
            Ok(())
        });
    }
    cmd.spawn()
        .context("could not start mpv (install it with: sudo pacman -S mpv yt-dlp)")
}

fn connect_with_retry(path: &Path) -> Option<UnixStream> {
    for _ in 0..60 {
        if let Ok(sock) = UnixStream::connect(path) {
            return Some(sock);
        }
        thread::sleep(Duration::from_millis(50));
    }
    None
}

fn forward_events<T: From<PlayerEvent>>(reader: UnixStream, tx: &Sender<T>) {
    for line in BufReader::new(reader).lines() {
        let Ok(line) = line else { break };
        let Some(event) = parse_event(&line) else {
            continue;
        };
        if tx.send(event.into()).is_err() {
            return;
        }
    }
    let _ = tx.send(PlayerEvent::Exited.into());
}

fn parse_event(line: &str) -> Option<PlayerEvent> {
    let v: Value = serde_json::from_str(line).ok()?;
    match v["event"].as_str()? {
        "property-change" => parse_property(v["name"].as_str()?, &v["data"]),
        "end-file" if v["reason"] == "error" => Some(PlayerEvent::PlaybackError),
        _ => None,
    }
}

fn parse_property(name: &str, data: &Value) -> Option<PlayerEvent> {
    let event = match name {
        "time-pos" => PlayerEvent::TimePos(data.as_f64()),
        "duration" => PlayerEvent::Duration(data.as_f64()),
        "pause" => PlayerEvent::Paused(data.as_bool().unwrap_or(false)),
        "volume" => PlayerEvent::Volume(data.as_f64()?),
        "playlist-pos" => {
            PlayerEvent::PlaylistPos(data.as_i64().and_then(|p| usize::try_from(p).ok()))
        }
        _ => return None,
    };
    Some(event)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_property_changes() {
        let line = r#"{"event":"property-change","id":1,"name":"time-pos","data":12.5}"#;
        assert_eq!(parse_event(line), Some(PlayerEvent::TimePos(Some(12.5))));

        let line = r#"{"event":"property-change","id":4,"name":"volume","data":80.0}"#;
        assert_eq!(parse_event(line), Some(PlayerEvent::Volume(80.0)));
    }

    #[test]
    fn negative_playlist_pos_means_nothing_playing() {
        let line = r#"{"event":"property-change","id":5,"name":"playlist-pos","data":-1}"#;
        assert_eq!(parse_event(line), Some(PlayerEvent::PlaylistPos(None)));
    }

    #[test]
    fn only_errors_are_reported_from_end_file() {
        let error = r#"{"event":"end-file","reason":"error"}"#;
        let eof = r#"{"event":"end-file","reason":"eof"}"#;
        assert_eq!(parse_event(error), Some(PlayerEvent::PlaybackError));
        assert_eq!(parse_event(eof), None);
    }

    #[test]
    fn ignores_command_replies_and_garbage() {
        assert_eq!(parse_event(r#"{"error":"success","request_id":0}"#), None);
        assert_eq!(parse_event("not json"), None);
    }
}
