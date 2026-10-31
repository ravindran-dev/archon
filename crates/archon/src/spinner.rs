//! A quiet progress indicator for steps that would otherwise dump a wall of
//! subprocess text (`git clone`, `makepkg`). Uses a restrained Braille spinner
//! on a terminal and stays silent in piped output and CI logs.

use std::io::IsTerminal;
use std::time::Duration;

use indicatif::{ProgressBar, ProgressDrawTarget, ProgressStyle};

const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

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
        let template = if std::env::var_os("NO_COLOR").is_some() {
            "{spinner} [{elapsed_precise}] {msg}"
        } else {
            "{spinner:.cyan} [{elapsed_precise}] {msg}"
        };
        bar.set_style(
            ProgressStyle::with_template(template)
                .expect("static spinner template is valid")
                .tick_strings(SPINNER_FRAMES),
        );
        bar.set_message(label.into());
        bar.enable_steady_tick(Duration::from_millis(100));
        Self { bar }
    }

    /// Two real makepkg stages; compilation itself has no universal percent.
    pub fn build(label: impl Into<String>, completed_stages: u64) -> Self {
        let spinner = Self::start(label);
        spinner.bar.set_length(2);
        spinner.bar.set_position(completed_stages);
        spinner.bar.set_style(
            ProgressStyle::with_template(
                "{spinner} [{elapsed_precise}] [{bar:16}] {pos}/{len} stages · {msg}",
            )
            .expect("static build template is valid")
            .tick_strings(SPINNER_FRAMES),
        );
        spinner
    }

    pub fn set_message(&self, message: impl Into<String>) {
        self.bar.set_message(message.into());
    }

    /// Stops the spinner and prints a green checkmark line in its place.
    pub fn success(self, msg: impl AsRef<str>) {
        self.bar.finish_and_clear();
        println!("{} {}", crate::color::bold_green("✓"), msg.as_ref());
    }

    /// Stops the spinner and prints a red cross line in its place. Use
    /// when the underlying step failed — callers still print full details
    /// (e.g. captured subprocess output) separately.
    pub fn fail(self, msg: impl AsRef<str>) {
        self.bar.finish_and_clear();
        println!("{} {}", crate::color::red("✗"), msg.as_ref());
    }
}
