mod api;
mod app;
mod player;
mod ui;

use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use anyhow::{Context, Result};
use ratatui::crossterm::event::{self, Event, KeyEventKind};
use ratatui::DefaultTerminal;

use api::{Filter, Track};
use app::{App, Msg};
use player::Player;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("--selftest") => return selftest(&args[1..].join(" ")),
        Some("-h" | "--help") => {
            println!("ytm - YouTube Music in the terminal, no account needed.\n");
            println!("Usage: ytm               start the player");
            println!("       ytm --selftest Q  check search + radio against YouTube Music");
            return Ok(());
        }
        _ => {}
    }

    let (tx, rx) = mpsc::channel();
    let player = Player::spawn(tx.clone())?;
    let mut app = App::new(player, tx);

    let mut terminal = ratatui::init();
    let res = run(&mut terminal, &mut app, &rx);
    ratatui::restore();
    res
}

fn run(terminal: &mut DefaultTerminal, app: &mut App, rx: &Receiver<Msg>) -> Result<()> {
    while !app.quit {
        terminal.draw(|f| ui::draw(f, app))?;
        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    app.on_key(key);
                }
            }
        }
        while let Ok(msg) = rx.try_recv() {
            app.on_msg(msg);
        }
    }
    Ok(())
}

fn selftest(query: &str) -> Result<()> {
    let query = if query.is_empty() { "daft punk" } else { query };
    for filter in [Filter::Songs, Filter::Videos] {
        let tracks = api::search(query, filter)?;
        println!("search {filter:?} '{query}': {} results", tracks.len());
        print_tracks(&tracks);
    }

    let seed = api::search(query, Filter::Songs)?
        .into_iter()
        .next()
        .context("no results to seed radio")?;
    let page = api::radio(&seed.id)?;
    println!(
        "radio from '{}': {} tracks, continuation: {}",
        seed.title,
        page.tracks.len(),
        page.next.is_some()
    );
    print_tracks(&page.tracks);

    if let Some(token) = page.next {
        let more = api::radio_more(&token)?;
        println!(
            "radio page 2: {} tracks, continuation: {}",
            more.tracks.len(),
            more.next.is_some()
        );
    }
    Ok(())
}

fn print_tracks(tracks: &[Track]) {
    for t in tracks.iter().take(3) {
        println!(
            "  {} | {} | {} | {} | {}",
            t.id, t.title, t.artist, t.album, t.length
        );
    }
}
