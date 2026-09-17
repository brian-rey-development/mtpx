//! Colors for stderr output, all of which collapse to plain text when color is off.

use console::Style;

/// Paints text for stderr, or leaves it alone when color is off.
#[derive(Debug, Clone, Copy)]
pub struct Theme {
    color: bool,
}

impl Theme {
    pub const fn new(color: bool) -> Self {
        Self { color }
    }

    /// Green: a transfer that ended cleanly.
    pub fn ok(self, text: &str) -> String {
        self.paint(Style::new().green(), text)
    }

    /// Yellow: something worth a look that did not stop the run.
    pub fn warn(self, text: &str) -> String {
        self.paint(Style::new().yellow(), text)
    }

    /// Red: a failure.
    pub fn err(self, text: &str) -> String {
        self.paint(Style::new().red(), text)
    }

    /// Dim: noise the eye can skip, such as retries.
    pub fn dim(self, text: &str) -> String {
        self.paint(Style::new().dim(), text)
    }

    /// Bold: the one line to read, such as the plan.
    pub fn bold(self, text: &str) -> String {
        self.paint(Style::new().bold(), text)
    }

    // Forced styling: console detects stdout, but the decision came from stderr.
    fn paint(self, style: Style, text: &str) -> String {
        if !self.color {
            return text.to_owned();
        }
        style.force_styling(true).apply_to(text).to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_theme_returns_the_text_untouched() {
        let theme = Theme::new(false);
        assert_eq!(theme.ok("done"), "done");
        assert_eq!(theme.err("failed"), "failed");
        assert_eq!(theme.bold("plan"), "plan");
    }

    #[test]
    fn color_theme_wraps_the_text_in_escapes() {
        let painted = Theme::new(true).err("failed");
        assert!(painted.starts_with("\u{1b}["), "{painted:?}");
        assert!(painted.contains("failed"));
    }
}
