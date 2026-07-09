//! A fast, quiet progress indicator for steps that would otherwise dump a
//! wall of subprocess text (`git clone`, `makepkg`) — styled after
//! pacman.conf's classic `ILoveCandy` chomping animation. Ticks every
//! 80ms; disables itself automatically when stdout isn't a terminal (CI
//! logs, `| tee`, etc.), matching `crate::color`'s own `NO_COLOR`/TTY
//! rules.

use std::io::IsTerminal;
use std::sync::OnceLock;
use std::time::Duration;

use indicatif::{ProgressBar, ProgressDrawTarget, ProgressStyle};

/// A short window of dots with a pac-man munching across it, looping.
/// Plain frames (no color codes baked in) — color comes from the style
/// template instead, so it's skipped automatically alongside everything
/// else when color is disabled.
fn candy_frames() -> &'static [String] {
    static FRAMES: OnceLock<Vec<String>> = OnceLock::new();
    FRAMES.get_or_init(|| {
        const WIDTH: usize = 10;
        (0..WIDTH)
            .map(|i| {
                (0..WIDTH)
                    .map(|j| if j < i { ' ' } else if j == i { 'C' } else { '.' })
                    .collect()
            })
            .collect()
    })
}

pub struct Spinner {
    bar: ProgressBar,
}

impl Spinner {
    /// Starts an animated spinner with `label` as the trailing message.
    /// Draws nothing at all when stderr isn't a terminal (piped output,
    /// logs) — a scrolling wall of spinner frames in a log file helps no
    /// one.
    pub fn start(label: impl Into<String>) -> Self {
        let target = if std::io::stderr().is_terminal() {
            ProgressDrawTarget::stderr()
        } else {
            ProgressDrawTarget::hidden()
        };
        let bar = ProgressBar::with_draw_target(None, target);
        let frames: Vec<&str> = candy_frames().iter().map(String::as_str).collect();
        bar.set_style(
            ProgressStyle::with_template("{spinner:.yellow} {msg}")
                .unwrap()
                .tick_strings(&frames),
        );
        bar.set_message(label.into());
        bar.enable_steady_tick(Duration::from_millis(80));
        Self { bar }
    }

    /// Stops the spinner and prints a green checkmark line in its place.
    pub fn success(self, msg: impl AsRef<str>) {
        self.bar.finish_and_clear();
        println!("{} {}", crate::color::green("✓"), msg.as_ref());
    }

    /// Stops the spinner and prints a red cross line in its place. Use
    /// when the underlying step failed — callers still print full details
    /// (e.g. captured subprocess output) separately.
    pub fn fail(self, msg: impl AsRef<str>) {
        self.bar.finish_and_clear();
        println!("{} {}", crate::color::red("✗"), msg.as_ref());
    }
}
