//! Persisted application configuration.
//!
//! Stored as JSON under `$XDG_CONFIG_HOME/encrypted-note/config.json`
//! (falling back to `~/.config/encrypted-note/config.json`). We deliberately
//! avoid extra crates such as `directories` to keep the dependency tree small.

use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Base directory for app config/state.
pub fn dir() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("encrypted-note")
}

/// Current wall-clock time as seconds since the Unix epoch.
pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Config {
    /// The vault that was open last time the app ran.
    pub last_vault: Option<PathBuf>,
    /// Most recently opened vaults, newest first.
    #[serde(default)]
    pub recent_vaults: Vec<PathBuf>,
    /// Whether the live markdown preview is shown next to the editor.
    #[serde(default = "default_true")]
    pub show_preview: bool,
    /// Editor font size in points.
    #[serde(default = "default_font_size")]
    pub font_size: f32,

    /// Folder (relative to a vault root) for daily notes.
    #[serde(default = "default_daily")]
    pub daily_dir: String,
    /// Moment-style date format for daily note file names.
    #[serde(default = "default_daily_format")]
    pub daily_format: String,
    /// Optional template note (title or vault-relative path) for new daily notes.
    #[serde(default)]
    pub daily_template: String,
    /// Folder (relative to a vault root) holding note templates.
    #[serde(default = "default_templates")]
    pub templates_dir: String,
    /// Whether the Vim editing mode is enabled.
    #[serde(default)]
    pub vim: bool,
    /// Startup window mode: `"fullscreen"`, `"maximized"`, or `"windowed"`.
    #[serde(default = "default_window_mode")]
    pub window_mode: String,

    // --- Workspace state (restored on next launch) ---
    /// Paths of the notes that were open, in order.
    #[serde(default)]
    pub open_tabs: Vec<PathBuf>,
    /// Index of the active tab.
    #[serde(default)]
    pub active_tab: usize,
    /// Whether the right-hand panel was visible.
    #[serde(default = "default_true")]
    pub show_right: bool,

    // --- Login lockout state (persisted so restarting cannot bypass it) ---
    /// Consecutive failed unlock attempts since the last success/lockout.
    #[serde(default)]
    pub failed_attempts: u32,
    /// How many lockouts have happened; drives the exponential backoff.
    #[serde(default)]
    pub lockout_step: u32,
    /// Unix timestamp (seconds) until which login is blocked, if locked.
    #[serde(default)]
    pub lockout_until: Option<u64>,
}

fn default_true() -> bool {
    true
}

fn default_font_size() -> f32 {
    15.0
}

fn default_daily() -> String {
    "Daily".to_string()
}

fn default_daily_format() -> String {
    "YYYY-MM-DD".to_string()
}

fn default_templates() -> String {
    "Templates".to_string()
}

fn default_window_mode() -> String {
    "fullscreen".to_string()
}

impl Default for Config {
    fn default() -> Self {
        Config {
            last_vault: None,
            recent_vaults: Vec::new(),
            show_preview: true,
            font_size: default_font_size(),
            daily_dir: default_daily(),
            daily_format: default_daily_format(),
            daily_template: String::new(),
            templates_dir: default_templates(),
            vim: false,
            window_mode: default_window_mode(),
            open_tabs: Vec::new(),
            active_tab: 0,
            show_right: true,
            failed_attempts: 0,
            lockout_step: 0,
            lockout_until: None,
        }
    }
}

impl Config {
    /// Absolute path to the config file.
    pub fn file_path() -> PathBuf {
        dir().join("config.json")
    }

    /// Load the config from disk, returning defaults if it is missing or malformed.
    pub fn load() -> Self {
        let path = Self::file_path();
        match fs::read_to_string(&path) {
            Ok(contents) => serde_json::from_str(&contents).unwrap_or_default(),
            Err(_) => Config::default(),
        }
    }

    /// Persist the config to disk, creating the directory if needed.
    pub fn save(&self) {
        let path = Self::file_path();
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let _ = fs::write(path, json);
        }
    }

    /// Record a vault as the most recent one.
    pub fn remember_vault(&mut self, path: &std::path::Path) {
        self.recent_vaults.retain(|p| p != path);
        self.recent_vaults.insert(0, path.to_path_buf());
        self.recent_vaults.truncate(10);
        self.last_vault = Some(path.to_path_buf());
    }

    // ------------------------------------------------------------- lockout

    /// Duration of the Nth lockout, in minutes: 5, 10, 20, 40, ... (5 * 2^step).
    pub fn lockout_minutes(step: u32) -> u64 {
        // Cap the shift to avoid overflow; well past any practical wait.
        let shift = step.min(20);
        5u64.saturating_mul(1u64 << shift)
    }

    /// Remaining lockout time in seconds as of `now`, or `None` if not locked.
    pub fn lockout_remaining(&self, now: u64) -> Option<u64> {
        self.lockout_until
            .filter(|until| *until > now)
            .map(|until| until - now)
    }

    /// Clear the lockout/attempt state (call after a successful unlock).
    pub fn reset_lockout(&mut self) {
        self.failed_attempts = 0;
        self.lockout_step = 0;
        self.lockout_until = None;
    }

    /// Record a failed attempt. If this reaches the threshold, arm a lockout
    /// and advance the backoff step.
    pub fn register_failure(&mut self, threshold: u32) -> Option<u64> {
        let now = now_unix();
        self.failed_attempts += 1;
        if self.failed_attempts >= threshold {
            let minutes = Self::lockout_minutes(self.lockout_step);
            self.lockout_until = Some(now + minutes * 60);
            self.lockout_step = self.lockout_step.saturating_add(1);
            self.failed_attempts = 0;
            Some(minutes)
        } else {
            // Clear a stale lockout if any.
            self.lockout_until = None;
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lockout_backoff_doubles() {
        assert_eq!(Config::lockout_minutes(0), 5);
        assert_eq!(Config::lockout_minutes(1), 10);
        assert_eq!(Config::lockout_minutes(2), 20);
        assert_eq!(Config::lockout_minutes(3), 40);
        assert_eq!(Config::lockout_minutes(4), 80);
        // Extremely large steps must not panic/overflow.
        assert!(Config::lockout_minutes(1000) > 0);
    }

    #[test]
    fn failures_arm_lockout_after_threshold() {
        let mut cfg = Config::default();
        for _ in 0..4 {
            assert!(cfg.register_failure(5).is_none());
        }
        assert_eq!(cfg.failed_attempts, 4);
        // Fifth failure arms a 5-minute lockout.
        assert_eq!(cfg.register_failure(5), Some(5));
        assert_eq!(cfg.failed_attempts, 0);
        assert_eq!(cfg.lockout_step, 1);
        let now = now_unix();
        assert!(cfg.lockout_remaining(now).is_some());
        assert!(cfg.lockout_remaining(now + 6 * 60).is_none());
    }

    #[test]
    fn lockout_escalates_then_resets() {
        let mut cfg = Config::default();
        // First lockout after 5 failures -> 5 minutes.
        for _ in 0..4 {
            assert!(cfg.register_failure(5).is_none());
        }
        assert_eq!(cfg.register_failure(5), Some(5));
        assert_eq!(cfg.lockout_step, 1);
        // Next 5 failures -> 10 minutes.
        for _ in 0..4 {
            assert!(cfg.register_failure(5).is_none());
        }
        assert_eq!(cfg.register_failure(5), Some(10));
        assert_eq!(cfg.lockout_step, 2);
        cfg.reset_lockout();
        assert_eq!(cfg.lockout_step, 0);
        assert!(cfg.lockout_until.is_none());
    }
}
