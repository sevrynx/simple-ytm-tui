use std::sync::mpsc::Sender;
use std::thread;
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::widgets::ListState;

use crate::api::{self, Filter, RadioPage, Track};
use crate::player::{Player, PlayerEvent};

pub enum Msg {
    Player(PlayerEvent),
    Search(u64, Result<Vec<Track>, String>),
    Radio(u64, Result<RadioPage, String>),
}

impl From<PlayerEvent> for Msg {
    fn from(event: PlayerEvent) -> Self {
        Msg::Player(event)
    }
}

#[derive(PartialEq, Eq, Clone, Copy)]
pub enum Mode {
    Normal,
    Search,
}

#[derive(PartialEq, Eq, Clone, Copy)]
pub enum Focus {
    Results,
    Queue,
}

const RADIO_LOOKAHEAD: usize = 5;
const SEEK_STEP: f64 = 10.0;
const VOLUME_STEP: f64 = 5.0;
const RESTART_THRESHOLD: f64 = 5.0;

pub struct App {
    pub mode: Mode,
    pub focus: Focus,
    pub input: String,
    pub filter: Filter,
    pub last_query: String,

    pub results: Vec<Track>,
    pub results_state: ListState,
    pub searching: bool,
    search_gen: u64,

    pub queue: Vec<Track>,
    pub queue_state: ListState,

    pub pos: Option<usize>,
    pub time: Option<f64>,
    pub duration: Option<f64>,
    pub paused: bool,
    pub volume: f64,

    pub autoplay: bool,
    pub radio_busy: bool,
    radio_token: Option<String>,
    radio_gen: u64,

    pub status: String,
    status_shown: String,
    status_since: Instant,
    pub quit: bool,

    tx: Sender<Msg>,
    player: Player,
}

impl App {
    pub fn new(player: Player, tx: Sender<Msg>) -> Self {
        App {
            mode: Mode::Search,
            focus: Focus::Results,
            input: String::new(),
            filter: Filter::Songs,
            last_query: String::new(),
            results: Vec::new(),
            results_state: ListState::default(),
            searching: false,
            search_gen: 0,
            queue: Vec::new(),
            queue_state: ListState::default(),
            pos: None,
            time: None,
            duration: None,
            paused: false,
            volume: 100.0,
            autoplay: true,
            radio_busy: false,
            radio_token: None,
            radio_gen: 0,
            status: String::new(),
            status_shown: String::new(),
            status_since: Instant::now(),
            quit: false,
            tx,
            player,
        }
    }

    pub fn status_line(&mut self) -> Option<&str> {
        if self.status != self.status_shown {
            self.status_shown = self.status.clone();
            self.status_since = Instant::now();
        }
        let fresh = self.status_since.elapsed() < Duration::from_secs(5);
        (!self.status.is_empty() && (fresh || self.status.starts_with("Loading")))
            .then_some(self.status.as_str())
    }

    pub fn current(&self) -> Option<&Track> {
        self.pos.and_then(|p| self.queue.get(p))
    }

    pub fn on_key(&mut self, key: KeyEvent) {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.quit = true;
            return;
        }
        match self.mode {
            Mode::Search => self.on_search_key(key),
            Mode::Normal => self.on_normal_key(key),
        }
    }

    fn on_search_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => self.mode = Mode::Normal,
            KeyCode::Enter => self.submit_search(),
            KeyCode::Tab => self.filter = self.filter.toggled(),
            KeyCode::Backspace => {
                self.input.pop();
            }
            KeyCode::Char(c) => self.input.push(c),
            _ => {}
        }
    }

    fn on_normal_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Char('/' | 's') => self.open_search(),
            KeyCode::Tab => self.toggle_focus(),
            KeyCode::Char('f') => self.toggle_filter(),
            KeyCode::Down | KeyCode::Char('j') => self.move_sel(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_sel(-1),
            KeyCode::PageDown => self.move_sel(10),
            KeyCode::PageUp => self.move_sel(-10),
            KeyCode::Home | KeyCode::Char('g') => self.move_sel(isize::MIN),
            KeyCode::End | KeyCode::Char('G') => self.move_sel(isize::MAX),
            KeyCode::Enter => self.activate_selected(),
            KeyCode::Char('a') => self.queue_selected_next(),
            KeyCode::Char('d') | KeyCode::Delete if self.focus == Focus::Queue => {
                self.remove_selected();
            }
            KeyCode::Char(' ' | 'p') => self.player.toggle_pause(),
            KeyCode::Char('n') => self.player.next(),
            KeyCode::Char('b') => self.previous_or_restart(),
            KeyCode::Right | KeyCode::Char('l') => self.player.seek_by(SEEK_STEP),
            KeyCode::Left | KeyCode::Char('h') => self.player.seek_by(-SEEK_STEP),
            KeyCode::Char('+' | '=') => self.player.change_volume(VOLUME_STEP),
            KeyCode::Char('-') => self.player.change_volume(-VOLUME_STEP),
            KeyCode::Char('r') => self.toggle_autoplay(),
            _ => {}
        }
    }

    fn open_search(&mut self) {
        self.mode = Mode::Search;
        self.input.clear();
    }

    fn submit_search(&mut self) {
        let query = self.input.trim().to_owned();
        if query.is_empty() {
            return;
        }
        self.start_search(query);
        self.mode = Mode::Normal;
        self.focus = Focus::Results;
    }

    fn toggle_focus(&mut self) {
        self.focus = match self.focus {
            Focus::Results => Focus::Queue,
            Focus::Queue => Focus::Results,
        };
    }

    fn toggle_filter(&mut self) {
        self.filter = self.filter.toggled();
        if !self.last_query.is_empty() {
            self.start_search(self.last_query.clone());
        }
    }

    fn toggle_autoplay(&mut self) {
        self.autoplay = !self.autoplay;
        self.status = format!(
            "Radio autoplay {}",
            if self.autoplay { "on" } else { "off" }
        );
        self.maybe_extend_radio();
    }

    fn previous_or_restart(&mut self) {
        if self.time.unwrap_or(0.0) > RESTART_THRESHOLD {
            self.player.restart();
        } else {
            self.player.previous();
        }
    }

    fn move_sel(&mut self, delta: isize) {
        let (len, state) = match self.focus {
            Focus::Results => (self.results.len(), &mut self.results_state),
            Focus::Queue => (self.queue.len(), &mut self.queue_state),
        };
        if len == 0 {
            return;
        }
        let cur = state.selected().unwrap_or(0) as isize;
        let next = cur.saturating_add(delta).clamp(0, len as isize - 1);
        state.select(Some(next as usize));
    }

    fn selected_result(&self) -> Option<Track> {
        self.results_state
            .selected()
            .and_then(|i| self.results.get(i))
            .cloned()
    }

    fn activate_selected(&mut self) {
        match self.focus {
            Focus::Results => {
                if let Some(track) = self.selected_result() {
                    self.play_now(track);
                }
            }
            Focus::Queue => {
                if let Some(i) = self.queue_state.selected() {
                    self.player.jump_to(i);
                }
            }
        }
    }

    fn queue_selected_next(&mut self) {
        if let Some(track) = self.selected_result() {
            self.play_next(track);
        }
    }

    fn remove_selected(&mut self) {
        if let Some(i) = self.queue_state.selected() {
            self.remove(i);
        }
    }

    fn play_now(&mut self, track: Track) {
        self.player.play(&track.url());
        self.status = format!("Loading {}...", track.title);
        let seed = track.id.clone();
        self.queue = vec![track];
        self.queue_state.select(Some(0));
        self.pos = None;
        self.time = None;
        self.duration = None;
        self.radio_gen += 1;
        self.radio_token = None;
        self.radio_busy = false;
        if self.autoplay {
            self.fetch_radio(Some(seed), None);
        }
    }

    fn play_next(&mut self, track: Track) {
        let Some(pos) = self.pos else {
            self.play_now(track);
            return;
        };
        let at = (pos + 1).min(self.queue.len());
        self.player.insert_at(at, &track.url());
        self.status = format!("Playing next: {}", track.title);
        self.queue.insert(at, track);
    }

    fn remove(&mut self, i: usize) {
        if Some(i) == self.pos {
            self.status = "Can't remove the current track, press n to skip it".into();
            return;
        }
        if i >= self.queue.len() {
            return;
        }
        self.player.remove(i);
        self.queue.remove(i);
        if let Some(pos) = self.pos {
            if i < pos {
                self.pos = Some(pos - 1);
            }
        }
        let last = self.queue.len().saturating_sub(1);
        if self.queue_state.selected().is_some_and(|s| s > last) {
            self.queue_state.select(Some(last));
        }
    }

    fn append(&mut self, tracks: Vec<Track>) -> usize {
        let mut added = 0;
        for track in tracks {
            if self.queue.iter().any(|q| q.id == track.id) {
                continue;
            }
            self.player.append(&track.url());
            self.queue.push(track);
            added += 1;
        }
        added
    }

    fn start_search(&mut self, query: String) {
        self.search_gen += 1;
        let gen = self.search_gen;
        self.searching = true;
        self.last_query.clone_from(&query);
        self.status = format!(
            "Searching {} for '{query}'...",
            self.filter.label().to_lowercase()
        );
        let filter = self.filter;
        let tx = self.tx.clone();
        thread::spawn(move || {
            let res = api::search(&query, filter).map_err(|e| format!("{e:#}"));
            let _ = tx.send(Msg::Search(gen, res));
        });
    }

    fn fetch_radio(&mut self, seed: Option<String>, token: Option<String>) {
        self.radio_busy = true;
        let gen = self.radio_gen;
        let tx = self.tx.clone();
        thread::spawn(move || {
            let res = match (token, seed) {
                (Some(token), _) => api::radio_more(&token),
                (None, Some(id)) => api::radio(&id),
                (None, None) => return,
            };
            let _ = tx.send(Msg::Radio(gen, res.map_err(|e| format!("{e:#}"))));
        });
    }

    fn maybe_extend_radio(&mut self) {
        if !self.autoplay || self.radio_busy || self.queue.is_empty() {
            return;
        }
        let pos = self.pos.unwrap_or(0);
        if pos + RADIO_LOOKAHEAD < self.queue.len() {
            return;
        }
        match self.radio_token.take() {
            Some(token) => self.fetch_radio(None, Some(token)),
            None => {
                let last = self.queue.last().map(|t| t.id.clone());
                self.fetch_radio(last, None);
            }
        }
    }

    pub fn on_msg(&mut self, msg: Msg) {
        match msg {
            Msg::Search(gen, res) => self.on_search_result(gen, res),
            Msg::Radio(gen, res) => self.on_radio_page(gen, res),
            Msg::Player(event) => self.on_player(event),
        }
    }

    fn on_search_result(&mut self, gen: u64, res: Result<Vec<Track>, String>) {
        if gen != self.search_gen {
            return;
        }
        self.searching = false;
        match res {
            Ok(tracks) => {
                self.status = format!("{} results", tracks.len());
                self.results_state
                    .select(if tracks.is_empty() { None } else { Some(0) });
                self.results = tracks;
            }
            Err(e) => self.status = format!("Search failed: {e}"),
        }
    }

    fn on_radio_page(&mut self, gen: u64, res: Result<RadioPage, String>) {
        if gen != self.radio_gen {
            return;
        }
        self.radio_busy = false;
        match res {
            Ok(page) => {
                self.radio_token = page.next;
                let added = self.append(page.tracks);
                if added == 0 && self.radio_token.is_none() {
                    self.status = "Radio ran out of new tracks".into();
                } else {
                    self.maybe_extend_radio();
                }
            }
            Err(e) => self.status = format!("Radio failed: {e}"),
        }
    }

    fn on_player(&mut self, event: PlayerEvent) {
        match event {
            PlayerEvent::TimePos(time) => self.on_time(time),
            PlayerEvent::Duration(duration) => self.duration = duration,
            PlayerEvent::Paused(paused) => self.paused = paused,
            PlayerEvent::Volume(volume) => self.volume = volume,
            PlayerEvent::PlaylistPos(pos) => self.on_pos(pos),
            PlayerEvent::PlaybackError => {
                self.status = "Couldn't play that one, skipping".into();
            }
            PlayerEvent::Exited => {
                self.status = "mpv exited".into();
                self.quit = true;
            }
        }
    }

    fn on_time(&mut self, time: Option<f64>) {
        self.time = time;
        if time.is_some() && self.status.starts_with("Loading") {
            self.status.clear();
        }
    }

    fn on_pos(&mut self, pos: Option<usize>) {
        if pos == self.pos {
            return;
        }
        self.pos = pos;
        self.time = None;
        self.duration = None;
        if let Some(p) = pos {
            if let Some(track) = self.queue.get(p) {
                self.status = format!("Loading {}...", track.title);
            }
            if self.focus != Focus::Queue || self.mode == Mode::Search {
                self.queue_state.select(Some(p));
            }
        }
        self.maybe_extend_radio();
    }
}
