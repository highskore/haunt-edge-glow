//! Edge glow: a breathing hairline of orange along the top of your screen, over every app, while
//! any agent waits for you; gone the moment you answer. Haunt labels it with who has waited longest.
// wit-bindgen's flattened `notify` import has 14 params.
#![allow(clippy::too_many_arguments)]

wit_bindgen::generate!({ path: "wit/haunt-plugin.wit", world: "plugin" });

// Shared with the other fleet-following plugins; not every helper is used here.
#[allow(dead_code)]
mod fleet;

use std::sync::Mutex;

use fleet::{Ev, Fleet};
use haunt::plugin::log::log;
use haunt::plugin::rows::Tone;
use haunt::plugin::types::LogLevel;
use haunt::plugin::glow::{self as edge, GlowLabel};
use haunt::plugin::timer;
use serde::Deserialize;

/// A refused show is tried again this often.
const RETRY_SECS: u32 = 15;

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// At the glow's right end: "tab" (who has waited longest), "count" ("2 need you") or "none".
    pub label: String,
    pub unverified: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self { label: "tab".into(), unverified: false }
    }
}

#[derive(Default)]
struct Live {
    settings: Settings,
    fleet: Fleet,
    /// What is on screen: `Some(label)` while it glows.
    shown: Option<Label>,
    /// The last refusal, logged once; the timer retries.
    refused: Option<String>,
}

static LIVE: Mutex<Option<Live>> = Mutex::new(None);

fn with<R>(f: impl FnOnce(&mut Live) -> R) -> R {
    f(LIVE.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(Live::default))
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

/// The label Haunt writes; the plugin only says which kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Label {
    None,
    Needs,
    /// The tab that has waited longest.
    Row(String),
}

/// `None`: no glow.
pub fn wanted(fleet: &Fleet, s: &Settings) -> Option<Label> {
    let (surface, _) = fleet.longest_waiting()?;
    Some(match s.label.as_str() {
        "none" => Label::None,
        "count" => Label::Needs,
        _ => Label::Row(surface.to_owned()),
    })
}

fn refresh(l: &mut Live) {
    let want = wanted(&l.fleet, &l.settings);
    if want == l.shown && l.refused.is_none() {
        return;
    }
    match &want {
        // Too many shows a second, or another plugin has the glow.
        Some(label) => match edge::show(Tone::Warn, &label.into()) {
            Ok(()) => {
                if l.refused.take().is_some() {
                    timer::stop();
                }
            }
            Err(e) => {
                if l.refused.is_none() {
                    let _ = timer::every(RETRY_SECS);
                }
                if l.refused.as_ref() != Some(&e) {
                    log(LogLevel::Warn, &format!("edge-glow: not shown: {e}"));
                    l.refused = Some(e);
                }
                return;
            }
        },
        None => {
            edge::hide();
            if l.refused.take().is_some() {
                timer::stop();
            }
        }
    }
    l.shown = want;
}

impl From<&Label> for GlowLabel {
    fn from(l: &Label) -> Self {
        match l {
            Label::None => GlowLabel::None,
            Label::Needs => GlowLabel::Needs,
            Label::Row(s) => GlowLabel::Row(s.clone()),
        }
    }
}

struct Plugin;

impl Guest for Plugin {
    fn init(json: String) {
        with(|l| l.settings = serde_json::from_str(&json).unwrap_or_default());
    }

    fn on_settings_changed(json: String) {
        with(|l| {
            l.settings = serde_json::from_str(&json).unwrap_or_default();
            refresh(l);
        });
    }

    fn on_event(event: AgentEvent) {
        let Some(surface) = event.surface.as_deref() else { return };
        with(|l| {
            if !event.verified && !l.settings.unverified {
                return;
            }
            let ev = Ev { surface, kind: &event.kind, cwd: event.cwd.as_deref(), summary: event.summary.as_deref(), subagent: event.agent_id.is_some() };
            if l.fleet.apply(&ev, now_ms()).is_some() {
                refresh(l);
            }
        });
    }

    fn palette_items(_mode: String, _query: String) -> Vec<PaletteItem> {
        Vec::new()
    }

    fn on_action(_action_id: String, _payload: String) {}

    fn row_menu(_surface: String) -> Vec<MenuItem> {
        Vec::new()
    }

    fn on_row_moved(_surface: String, _group: String, _before: Option<String>) {}

    fn on_theme_changed() {}

    fn on_tick(_now_ms: u64) {
        with(refresh);
    }

    fn on_exec(_job: u64, _outcome: Result<haunt::plugin::exec::Output, String>) {}

    fn on_away_changed(_away: bool, _away_ms: u64) {}

    fn on_hover_hold(_held: bool, _surface: String) {}

    fn on_fs_tree(_job: u64, _outcome: Result<haunt::plugin::fs_list::TreeResult, String>) {}
}

export!(Plugin);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glows_with_who_waited_longest() {
        let mut f = Fleet::default();
        let s = Settings::default();
        assert_eq!(wanted(&f, &s), None);
        f.apply(&Ev { surface: "a", kind: "permission_seen", cwd: Some("/w/api"), ..Ev::default() }, 0);
        f.apply(&Ev { surface: "b", kind: "question_asked", cwd: Some("/w/web"), ..Ev::default() }, 5);
        assert_eq!(wanted(&f, &s), Some(Label::Row("a".into())), "Haunt writes \"api · git push? · 2m\"");
        let with = |label: &str| wanted(&f, &Settings { label: label.into(), ..s.clone() });
        assert_eq!(with("count"), Some(Label::Needs), "\"2 need you\"");
        assert_eq!(with("none"), Some(Label::None), "the glow alone");
    }

    #[test]
    fn manifest_asks_for_what_it_uses_and_defaults_match() {
        let m = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/haunt-plugin.toml")).unwrap();
        let t: toml::Table = toml::from_str(&m).unwrap();
        let caps = t["capabilities"].as_table().unwrap();
        assert_eq!(caps.keys().collect::<Vec<_>>(), ["event_details", "events", "glow", "timer"]);
        let defaults: serde_json::Map<String, serde_json::Value> =
            t["settings"].as_table().unwrap().iter().map(|(k, v)| (k.clone(), serde_json::to_value(&v["default"]).unwrap())).collect();
        assert_eq!(serde_json::from_value::<Settings>(serde_json::Value::Object(defaults)).unwrap(), Settings::default());
    }
}
