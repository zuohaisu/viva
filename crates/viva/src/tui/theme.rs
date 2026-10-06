//! The workbench chrome palette and status vocabulary (herdr-aligned look;
//! route A: our own implementation, no herdr code — see
//! docs/research/herdr-runtime-parity-2026-10-01.md §5/§7).
//!
//! Scope honesty: this is ONE fixed default palette, not a theme system.
//! The 2026-10-01 ruling keeps multi-theme/custom-TOML/auto-light-dark out
//! of the first version. What is adopted now is the *role structure*: every
//! chrome color is named for what it means (accent, overlay0, …), so a
//! theme system later swaps values without touching call sites.
//!
//! The values are the published Catppuccin Mocha colors (MIT-licensed
//! palette, not herdr material).

use ratatui::style::Color;

use crate::agents::AgentStatus;

/// Every chrome color, named by role. Terminal CONTENT is never styled by
/// this palette — it keeps the child's own colors; these roles style only
/// Viva's chrome (sidebar, borders, bars).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Palette {
    /// Primary accent: focused pane borders, focused section headers, mode
    /// chips.
    pub accent: Color,
    /// Background for bars and overlay cards.
    pub panel_bg: Color,
    /// Sidebar background. `Reset` preserves the terminal's own background.
    pub sidebar_bg: Color,
    /// Background of the focused workspace/agent row.
    pub active_row_bg: Color,
    /// Background of the selected (cursor) row in the sidebar.
    pub selection_bg: Color,
    /// Subtle surface for secondary surfaces.
    pub surface0: Color,
    /// Lighter surface for hover/active states.
    pub surface1: Color,
    /// Very dim surface for separators.
    pub surface_dim: Color,
    /// Muted text: secondary info, unverified markers, unfocused borders.
    pub overlay0: Color,
    /// Brighter overlay text.
    pub overlay1: Color,
    /// Main text color.
    pub text: Color,
    /// Subdued text: dim labels, detail lines.
    pub subtext0: Color,
    /// Branch names / special labels.
    pub mauve: Color,
    /// Idle / clean states.
    pub green: Color,
    /// Working / dirty states.
    pub yellow: Color,
    /// Blocked / needs-attention states.
    pub red: Color,
    /// Finished-notification accent.
    pub blue: Color,
    /// Done / unseen markers.
    pub teal: Color,
    /// Rate-limited / warning states.
    pub peach: Color,
}

impl Palette {
    /// The default: Catppuccin Mocha.
    pub fn catppuccin_mocha() -> Self {
        Self {
            accent: Color::Rgb(137, 180, 250), // blue
            panel_bg: Color::Rgb(24, 24, 37),  // mantle
            sidebar_bg: Color::Reset,
            active_row_bg: Color::Rgb(30, 30, 46), // base
            selection_bg: Color::Rgb(49, 50, 68),  // surface0
            surface0: Color::Rgb(49, 50, 68),
            surface1: Color::Rgb(69, 71, 90),    // surface1
            surface_dim: Color::Rgb(30, 30, 46), // base
            overlay0: Color::Rgb(108, 112, 134),
            overlay1: Color::Rgb(127, 132, 156),
            text: Color::Rgb(205, 214, 244),
            subtext0: Color::Rgb(166, 173, 200),
            mauve: Color::Rgb(203, 166, 247),
            green: Color::Rgb(166, 227, 161),
            yellow: Color::Rgb(249, 226, 175),
            red: Color::Rgb(243, 139, 168),
            blue: Color::Rgb(137, 180, 250),
            teal: Color::Rgb(148, 226, 213),
            peach: Color::Rgb(250, 179, 135),
        }
    }

    /// Readable foreground on an accent/colored chip background.
    pub fn chip_fg(&self) -> Color {
        Color::Rgb(30, 30, 46) // base — dark text on bright chip
    }
}

/// The status dot for an agent state (herdr's `dots` indicator style):
/// filled dot for active states, hollow for idle, middle dot for unknown.
pub fn status_glyph(status: AgentStatus) -> &'static str {
    match status {
        AgentStatus::Working | AgentStatus::Blocked | AgentStatus::Done => "●",
        AgentStatus::Idle => "○",
        AgentStatus::RateLimited | AgentStatus::Unknown => "·",
    }
}

/// The color of a status dot. Semantic mapping: working=yellow,
/// blocked=red, done=teal, idle=green, rate_limited=peach, unknown=muted.
pub fn status_color(status: AgentStatus, palette: &Palette) -> Color {
    match status {
        AgentStatus::Working => palette.yellow,
        AgentStatus::Blocked => palette.red,
        AgentStatus::Done => palette.teal,
        AgentStatus::Idle => palette.green,
        AgentStatus::RateLimited => palette.peach,
        AgentStatus::Unknown => palette.overlay0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_vocabulary_covers_every_agent_state() {
        let palette = Palette::catppuccin_mocha();
        // Filled dots for the three active states, hollow for idle, dim
        // middle dot for rate-limited/unknown.
        assert_eq!(status_glyph(AgentStatus::Working), "●");
        assert_eq!(status_glyph(AgentStatus::Blocked), "●");
        assert_eq!(status_glyph(AgentStatus::Done), "●");
        assert_eq!(status_glyph(AgentStatus::Idle), "○");
        assert_eq!(status_glyph(AgentStatus::RateLimited), "·");
        assert_eq!(status_glyph(AgentStatus::Unknown), "·");
        // Colors are distinct per semantic role.
        assert_eq!(status_color(AgentStatus::Working, &palette), palette.yellow);
        assert_eq!(status_color(AgentStatus::Blocked, &palette), palette.red);
        assert_eq!(status_color(AgentStatus::Done, &palette), palette.teal);
        assert_eq!(status_color(AgentStatus::Idle, &palette), palette.green);
        assert_eq!(
            status_color(AgentStatus::RateLimited, &palette),
            palette.peach
        );
        assert_eq!(
            status_color(AgentStatus::Unknown, &palette),
            palette.overlay0
        );
    }
}
