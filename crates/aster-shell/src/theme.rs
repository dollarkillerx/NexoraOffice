//! Design tokens: the one place colours, spacing, radii and type sizes are
//! written down.
//!
//! The Slint side declares a global with the same names and the binary copies
//! these values into it; the custom-drawn document surface takes its palette
//! from here too, so chrome and content cannot drift apart. Starting values come
//! from the plan's "界面设计方向": neutral greys, hairline separators, controls
//! 28 px high with 6 px corners, and violet as the only hue.

/// A colour as straight 8-bit RGBA.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgba(pub u8, pub u8, pub u8, pub u8);

impl Rgba {
    pub const fn rgb(hex: u32) -> Self {
        Self((hex >> 16) as u8, (hex >> 8) as u8, hex as u8, 0xff)
    }

    /// The same colour at an opacity between 0 and 1.
    pub const fn with_alpha(self, alpha: f32) -> Self {
        Self(self.0, self.1, self.2, (alpha * 255.0 + 0.5) as u8)
    }

    pub const fn to_array(self) -> [u8; 4] {
        [self.0, self.1, self.2, self.3]
    }

    /// This colour painted over an opaque background.
    pub fn over(self, background: Rgba) -> Rgba {
        let alpha = f32::from(self.3) / 255.0;
        let mix = |top: u8, bottom: u8| {
            (f32::from(top) * alpha + f32::from(bottom) * (1.0 - alpha)).round() as u8
        };
        Rgba(
            mix(self.0, background.0),
            mix(self.1, background.1),
            mix(self.2, background.2),
            0xff,
        )
    }

    /// WCAG relative luminance of an opaque colour.
    pub fn luminance(self) -> f32 {
        let channel = |value: u8| {
            let v = f32::from(value) / 255.0;
            if v <= 0.04045 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * channel(self.0) + 0.7152 * channel(self.1) + 0.0722 * channel(self.2)
    }

    /// Difference between the largest and the smallest colour channel: zero for
    /// a pure grey, large for a saturated colour.
    pub fn chroma(self) -> u8 {
        let max = self.0.max(self.1).max(self.2);
        let min = self.0.min(self.1).min(self.2);
        max - min
    }
}

/// WCAG contrast ratio between a foreground and an opaque background. A
/// translucent foreground is composited first.
pub fn contrast(foreground: Rgba, background: Rgba) -> f32 {
    let a = foreground.over(background).luminance();
    let b = background.luminance();
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

/// The single accent: violet. Also the brand colour of the slide block in the logo.
pub const ACCENT: Rgba = Rgba::rgb(0x7c5ce0);
/// Brand near-black.
pub const INK: Rgba = Rgba::rgb(0x0b0b0b);
/// Brand warm white.
pub const PAPER: Rgba = Rgba::rgb(0xf7f7f6);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ThemeMode {
    Light,
    Dark,
}

impl ThemeMode {
    pub fn toggled(self) -> Self {
        match self {
            Self::Light => Self::Dark,
            Self::Dark => Self::Light,
        }
    }
}

/// Every colour the shell and the document surface use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Colors {
    /// Base of the chrome: toolbar row, active tab, cards.
    pub surface: Rgba,
    /// One step back from `surface`: the tab strip.
    pub sunken: Rgba,
    /// Side panels and the status bar.
    pub panel: Rgba,
    /// Filled controls: the toolbar capsule, input fields.
    pub fill: Rgba,
    /// Overlay on a hovered control.
    pub hover: Rgba,
    /// Overlay on a pressed or selected control.
    pub pressed: Rgba,
    /// Separators between regions.
    pub hairline: Rgba,
    pub text: Rgba,
    pub text_secondary: Rgba,
    pub text_tertiary: Rgba,
    /// Text and icons of controls that do nothing yet.
    pub text_disabled: Rgba,
    pub accent: Rgba,
    /// Text and icons on top of the accent.
    pub on_accent: Rgba,
    /// Tinted background for selected items.
    pub accent_soft: Rgba,
    /// Background of the document area, around the page.
    pub desk: Rgba,
    pub page: Rgba,
    pub page_edge: Rgba,
    /// Text on the page.
    pub ink: Rgba,
    /// Selected text on the page. Opaque: the surface paints it under the glyphs.
    pub selection: Rgba,
    pub caret: Rgba,
    /// Links in documents: the accent, lightened on dark backgrounds so it
    /// stays readable as text.
    pub link: Rgba,
}

impl Colors {
    pub const fn light() -> Self {
        Self {
            surface: Rgba::rgb(0xffffff),
            sunken: Rgba::rgb(0xececeb),
            panel: PAPER,
            fill: Rgba::rgb(0xf1f1f0),
            hover: INK.with_alpha(0.06),
            pressed: INK.with_alpha(0.11),
            hairline: INK.with_alpha(0.12),
            text: INK,
            text_secondary: Rgba::rgb(0x5a5a58),
            text_tertiary: Rgba::rgb(0x767674),
            text_disabled: INK.with_alpha(0.30),
            accent: ACCENT,
            on_accent: Rgba::rgb(0xffffff),
            accent_soft: ACCENT.with_alpha(0.14),
            desk: Rgba::rgb(0xf1f1f0),
            page: Rgba::rgb(0xffffff),
            page_edge: Rgba::rgb(0xdededd),
            ink: INK,
            selection: Rgba::rgb(0xddd5f7),
            caret: ACCENT,
            link: ACCENT,
        }
    }

    pub const fn dark() -> Self {
        Self {
            surface: Rgba::rgb(0x1e1e1d),
            sunken: Rgba::rgb(0x0f0f0e),
            panel: Rgba::rgb(0x171716),
            fill: Rgba::rgb(0x2b2b2a),
            hover: PAPER.with_alpha(0.08),
            pressed: PAPER.with_alpha(0.14),
            hairline: PAPER.with_alpha(0.12),
            text: PAPER,
            text_secondary: Rgba::rgb(0xa9a9a6),
            text_tertiary: Rgba::rgb(0x8a8a87),
            text_disabled: PAPER.with_alpha(0.30),
            accent: ACCENT,
            on_accent: Rgba::rgb(0xffffff),
            accent_soft: ACCENT.with_alpha(0.26),
            desk: Rgba::rgb(0x121211),
            page: Rgba::rgb(0x1e1e1d),
            page_edge: Rgba::rgb(0x333332),
            ink: PAPER,
            selection: Rgba::rgb(0x40346f),
            caret: Rgba::rgb(0x9a80ee),
            link: Rgba::rgb(0x9a80ee),
        }
    }

    pub const fn of(mode: ThemeMode) -> Self {
        match mode {
            ThemeMode::Light => Self::light(),
            ThemeMode::Dark => Self::dark(),
        }
    }

    /// Every colour with its token name, for checks that must cover all of them.
    pub fn named(&self) -> [(&'static str, Rgba); 21] {
        [
            ("surface", self.surface),
            ("sunken", self.sunken),
            ("panel", self.panel),
            ("fill", self.fill),
            ("hover", self.hover),
            ("pressed", self.pressed),
            ("hairline", self.hairline),
            ("text", self.text),
            ("text-secondary", self.text_secondary),
            ("text-tertiary", self.text_tertiary),
            ("text-disabled", self.text_disabled),
            ("accent", self.accent),
            ("on-accent", self.on_accent),
            ("accent-soft", self.accent_soft),
            ("desk", self.desk),
            ("page", self.page),
            ("page-edge", self.page_edge),
            ("ink", self.ink),
            ("selection", self.selection),
            ("caret", self.caret),
            ("link", self.link),
        ]
    }
}

/// Bars and titles of GitHub's five callouts in documents: note, tip,
/// important, warning and caution. GitHub's own colours, which readers of
/// Markdown know, except that "important" takes the accent. All of them are
/// text and keep at least 4.5:1 against the page.
pub const fn callout_colors(mode: ThemeMode) -> [Rgba; 5] {
    match mode {
        ThemeMode::Light => [
            Rgba::rgb(0x0969da),
            Rgba::rgb(0x1a7f37),
            ACCENT,
            Rgba::rgb(0x9a6700),
            Rgba::rgb(0xd1242f),
        ],
        ThemeMode::Dark => [
            Rgba::rgb(0x4493f8),
            Rgba::rgb(0x3fb950),
            Rgba::rgb(0x9a80ee),
            Rgba::rgb(0xd29922),
            Rgba::rgb(0xf85149),
        ],
    }
}

/// Lengths in logical pixels. The same in both themes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Metrics {
    pub space_xs: f32,
    pub space_sm: f32,
    pub space_md: f32,
    pub space_lg: f32,
    pub space_xl: f32,
    pub radius_sm: f32,
    pub radius_md: f32,
    pub radius_lg: f32,
    /// Height of buttons, fields and list rows.
    pub control_height: f32,
    pub tab_strip_height: f32,
    pub toolbar_height: f32,
    pub status_bar_height: f32,
    pub sidebar_width: f32,
    pub icon_size: f32,
    pub font_xs: f32,
    pub font_sm: f32,
    pub font_md: f32,
    pub font_lg: f32,
    pub font_xl: f32,
    pub font_display: f32,
}

impl Metrics {
    pub const fn standard() -> Self {
        Self {
            space_xs: 4.0,
            space_sm: 8.0,
            space_md: 12.0,
            space_lg: 16.0,
            space_xl: 24.0,
            radius_sm: 4.0,
            radius_md: 6.0,
            radius_lg: 10.0,
            control_height: 28.0,
            tab_strip_height: 40.0,
            toolbar_height: 36.0,
            status_bar_height: 26.0,
            sidebar_width: 232.0,
            icon_size: 16.0,
            font_xs: 11.0,
            font_sm: 12.0,
            font_md: 13.0,
            font_lg: 15.0,
            font_xl: 18.0,
            font_display: 26.0,
        }
    }
}

/// Page geometry of the Writer view, in logical pixels at 100 % zoom. An A4
/// sheet at 96 dpi with 25 mm margins.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PageMetrics {
    pub desk_margin: f32,
    pub width: f32,
    pub padding: f32,
    /// Height over width.
    pub aspect: f32,
}

impl PageMetrics {
    pub const fn a4() -> Self {
        Self {
            desk_margin: 24.0,
            width: 794.0,
            padding: 94.0,
            aspect: 297.0 / 210.0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tokens {
    pub mode: ThemeMode,
    pub colors: Colors,
    pub metrics: Metrics,
    pub page: PageMetrics,
}

impl Tokens {
    pub const fn of(mode: ThemeMode) -> Self {
        Self {
            mode,
            colors: Colors::of(mode),
            metrics: Metrics::standard(),
            page: PageMetrics::a4(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MODES: [ThemeMode; 2] = [ThemeMode::Light, ThemeMode::Dark];

    /// Tokens that are allowed to carry the accent hue.
    const VIOLET_TOKENS: [&str; 5] = ["accent", "accent-soft", "selection", "caret", "link"];

    #[test]
    fn hex_and_alpha_helpers() {
        assert_eq!(Rgba::rgb(0x7c5ce0), Rgba(0x7c, 0x5c, 0xe0, 0xff));
        assert_eq!(INK.with_alpha(0.5).3, 128);
        assert_eq!(
            Rgba(0, 0, 0, 128).over(Rgba::rgb(0xffffff)),
            Rgba(127, 127, 127, 255)
        );
    }

    #[test]
    fn callouts_are_readable_on_the_page() {
        for mode in MODES {
            let page = Colors::of(mode).page;
            for color in callout_colors(mode) {
                let ratio = contrast(color, page);
                assert!(ratio >= 4.5, "{mode:?} {color:?}: {ratio}");
            }
        }
    }

    #[test]
    fn contrast_matches_the_brand_sheet() {
        // The figures quoted in assets/brand/README.md.
        let on_white = contrast(ACCENT, Rgba::rgb(0xffffff));
        let on_ink = contrast(ACCENT, INK);
        assert!((on_white - 4.70).abs() < 0.02, "{on_white}");
        assert!((on_ink - 4.18).abs() < 0.02, "{on_ink}");
    }

    #[test]
    fn the_accent_is_the_same_violet_in_both_themes() {
        for mode in MODES {
            assert_eq!(Colors::of(mode).accent, ACCENT);
        }
    }

    #[test]
    fn violet_is_the_only_hue() {
        for mode in MODES {
            for (name, value) in Colors::of(mode).named() {
                if VIOLET_TOKENS.contains(&name) {
                    // Violet: blue above red above green.
                    assert!(
                        value.2 > value.0 && value.0 > value.1,
                        "{mode:?} {name} {value:?} is not violet"
                    );
                } else {
                    // Warm white is the least neutral grey we use.
                    assert!(
                        value.chroma() <= PAPER.chroma() + 2,
                        "{mode:?} {name} {value:?} has a hue"
                    );
                }
            }
        }
    }

    #[test]
    fn text_is_readable_on_every_background() {
        for mode in MODES {
            let c = Colors::of(mode);
            for (name, background) in [
                ("surface", c.surface),
                ("sunken", c.sunken),
                ("panel", c.panel),
                ("fill", c.fill),
            ] {
                let primary = contrast(c.text, background);
                let secondary = contrast(c.text_secondary, background);
                let tertiary = contrast(c.text_tertiary, background);
                assert!(primary >= 12.0, "{mode:?} text on {name}: {primary}");
                assert!(
                    secondary >= 4.5,
                    "{mode:?} secondary on {name}: {secondary}"
                );
                assert!(tertiary >= 3.0, "{mode:?} tertiary on {name}: {tertiary}");
            }
            assert!(contrast(c.ink, c.page) >= 12.0);
            assert!(contrast(c.on_accent, c.accent) >= 4.5);
            assert!(contrast(c.caret, c.page) >= 4.0, "{mode:?} caret");
            // Links are read as text, on the editor surface and on code.
            assert!(contrast(c.link, c.surface) >= 4.5, "{mode:?} link");
            assert!(contrast(c.link, c.fill) >= 4.0, "{mode:?} link on fill");
            // Selected text stays readable.
            assert!(contrast(c.ink, c.selection) >= 7.0, "{mode:?} selection");
        }
    }

    #[test]
    fn disabled_controls_look_disabled_but_stay_visible() {
        for mode in MODES {
            let c = Colors::of(mode);
            let disabled = contrast(c.text_disabled, c.fill);
            let enabled = contrast(c.text_secondary, c.fill);
            assert!(
                disabled >= 1.5,
                "{mode:?} disabled is invisible: {disabled}"
            );
            assert!(
                disabled * 2.0 < enabled,
                "{mode:?} disabled ({disabled}) is too close to enabled ({enabled})"
            );
        }
    }

    #[test]
    fn regions_are_told_apart() {
        for mode in MODES {
            let c = Colors::of(mode);
            assert_ne!(c.surface, c.sunken, "{mode:?} active tab vs strip");
            assert_ne!(c.page, c.desk, "{mode:?} page vs desk");
            // Colours handed to the document surface must be opaque.
            for value in [
                c.desk,
                c.page,
                c.page_edge,
                c.ink,
                c.selection,
                c.caret,
                c.link,
            ] {
                assert_eq!(value.3, 0xff);
            }
        }
    }

    #[test]
    fn dark_is_dark_and_light_is_light() {
        let light = Colors::light();
        let dark = Colors::dark();
        assert!(light.surface.luminance() > 0.8);
        assert!(dark.surface.luminance() < 0.05);
        assert_eq!(ThemeMode::Light.toggled(), ThemeMode::Dark);
        assert_eq!(ThemeMode::Dark.toggled().toggled(), ThemeMode::Dark);
    }

    #[test]
    fn metrics_follow_the_plan() {
        let m = Metrics::standard();
        assert_eq!(m.control_height, 28.0);
        assert_eq!(m.radius_md, 6.0);
        // A control fits in the toolbar capsule and the tab strip.
        assert!(m.control_height < m.toolbar_height);
        assert!(m.control_height < m.tab_strip_height);
        // The type scale and the spacing scale only go up.
        let fonts = [
            m.font_xs,
            m.font_sm,
            m.font_md,
            m.font_lg,
            m.font_xl,
            m.font_display,
        ];
        assert!(fonts.windows(2).all(|pair| pair[0] < pair[1]));
        let spaces = [m.space_xs, m.space_sm, m.space_md, m.space_lg, m.space_xl];
        assert!(spaces.windows(2).all(|pair| pair[0] < pair[1]));
    }
}
