use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::{json, Value};

const BASE: &str = "https://music.youtube.com/youtubei/v1/";
const CLIENT_VERSION: &str = "1.20250101.01.00";
const USER_AGENT: &str = "Mozilla/5.0 (X11; Linux x86_64; rv:130.0) Gecko/20100101 Firefox/130.0";

#[derive(Clone, Debug)]
pub struct Track {
    pub id: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub length: String,
}

impl Track {
    pub fn url(&self) -> String {
        format!("https://www.youtube.com/watch?v={}", self.id)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Filter {
    Songs,
    Videos,
}

impl Filter {
    pub fn toggled(self) -> Self {
        match self {
            Filter::Songs => Filter::Videos,
            Filter::Videos => Filter::Songs,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Filter::Songs => "Songs",
            Filter::Videos => "Videos",
        }
    }

    fn search_params(self) -> &'static str {
        match self {
            Filter::Songs => "EgWKAQIIAWoMEA4QChADEAQQCRAF",
            Filter::Videos => "EgWKAQIQAWoMEA4QChADEAQQCRAF",
        }
    }
}

#[derive(Debug)]
pub struct RadioPage {
    pub tracks: Vec<Track>,
    pub next: Option<String>,
}

pub fn search(query: &str, filter: Filter) -> Result<Vec<Track>> {
    let body = json!({ "query": query, "params": filter.search_params() });
    let v = call("search", body)?;

    let mut out = Vec::new();
    walk(&v, &mut |key, val| {
        if key == "musicResponsiveListItemRenderer" {
            out.extend(parse_list_item(val));
            return false;
        }
        true
    });
    Ok(out)
}

pub fn radio(seed_id: &str) -> Result<RadioPage> {
    let body = json!({
        "videoId": seed_id,
        "playlistId": format!("RDAMVM{seed_id}"),
        "isAudioOnly": true,
    });
    Ok(parse_next(&call("next", body)?))
}

pub fn radio_more(token: &str) -> Result<RadioPage> {
    Ok(parse_next(&call("next", json!({ "continuation": token }))?))
}

fn call(endpoint: &str, mut body: Value) -> Result<Value> {
    body["context"] = json!({
        "client": {
            "clientName": "WEB_REMIX",
            "clientVersion": CLIENT_VERSION,
            "hl": "en",
            "gl": "US",
        }
    });
    let resp = ureq::post(&format!("{BASE}{endpoint}?prettyPrint=false"))
        .set("Origin", "https://music.youtube.com")
        .set("User-Agent", USER_AGENT)
        .timeout(Duration::from_secs(15))
        .send_json(body)
        .with_context(|| format!("YouTube Music {endpoint} request failed"))?;
    resp.into_json()
        .with_context(|| format!("YouTube Music {endpoint} returned bad JSON"))
}

fn parse_next(v: &Value) -> RadioPage {
    let mut tracks = Vec::new();
    let mut next = None;
    walk(v, &mut |key, val| match key {
        "playlistPanelVideoWrapperRenderer" => {
            tracks.extend(parse_panel_item(
                &val["primaryRenderer"]["playlistPanelVideoRenderer"],
            ));
            false
        }
        "playlistPanelVideoRenderer" => {
            tracks.extend(parse_panel_item(val));
            false
        }
        "nextRadioContinuationData" | "nextContinuationData" => {
            if next.is_none() {
                next = val["continuation"].as_str().map(str::to_owned);
            }
            false
        }
        _ => true,
    });
    RadioPage { tracks, next }
}

fn parse_list_item(r: &Value) -> Option<Track> {
    let id = r["playlistItemData"]["videoId"]
        .as_str()
        .map(str::to_owned)
        .or_else(|| find_str(r, "videoId"))?;
    let cols: Vec<String> = r["flexColumns"]
        .as_array()?
        .iter()
        .map(|c| runs_text(&c["musicResponsiveListItemFlexColumnRenderer"]["text"]))
        .collect();
    let title = cols.first()?.clone();
    let mut parts: Vec<&str> = cols
        .get(1)
        .map(|s| s.split(" • ").collect())
        .unwrap_or_default();
    if matches!(parts.first(), Some(&("Song" | "Video"))) {
        parts.remove(0);
    }
    let length = match parts.last() {
        Some(p) if is_duration(p) => parts.pop().unwrap_or_default().to_owned(),
        _ => String::new(),
    };
    let artist = parts.first().copied().unwrap_or_default().to_owned();
    let album = parts
        .iter()
        .skip(1)
        .filter(|p| !p.contains(" views") && !p.contains(" plays"))
        .copied()
        .collect::<Vec<_>>()
        .join(" • ");
    Some(Track {
        id,
        title,
        artist,
        album,
        length,
    })
}

fn parse_panel_item(r: &Value) -> Option<Track> {
    let id = r["videoId"].as_str()?.to_owned();
    let title = runs_text(&r["title"]);
    let byline = runs_text(&r["longBylineText"]);
    let parts: Vec<&str> = byline.split(" • ").collect();
    let artist = parts.first().copied().unwrap_or_default().to_owned();
    let album = match parts.get(1) {
        Some(p) if !p.chars().all(|c| c.is_ascii_digit()) => (*p).to_owned(),
        _ => String::new(),
    };
    let length = runs_text(&r["lengthText"]);
    Some(Track {
        id,
        title,
        artist,
        album,
        length,
    })
}

fn runs_text(v: &Value) -> String {
    v["runs"]
        .as_array()
        .map(|runs| runs.iter().filter_map(|r| r["text"].as_str()).collect())
        .unwrap_or_default()
}

fn is_duration(s: &str) -> bool {
    s.contains(':') && s.chars().all(|c| c.is_ascii_digit() || c == ':')
}

fn walk(v: &Value, f: &mut dyn FnMut(&str, &Value) -> bool) {
    match v {
        Value::Object(map) => {
            for (k, val) in map {
                if f(k, val) {
                    walk(val, f);
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                walk(item, f);
            }
        }
        _ => {}
    }
}

fn find_str(v: &Value, key: &str) -> Option<String> {
    let mut found = None;
    walk(v, &mut |k, val| {
        if found.is_some() {
            return false;
        }
        if k == key {
            found = val.as_str().map(str::to_owned);
        }
        true
    });
    found
}
