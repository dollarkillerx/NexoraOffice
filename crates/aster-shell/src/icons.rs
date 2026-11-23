//! Line icons, as SVG path data on a 20 × 20 grid.
//!
//! They are drawn for this project, stroked at 1.5 px with round caps and never
//! filled, so the UI layer can colour them from the design tokens. The plan names
//! Material Symbols as the eventual icon set; that needs a licence review first,
//! and these keep the shell self-contained until then.

/// Side of the square every icon is drawn on.
pub const GRID: f32 = 20.0;

macro_rules! icons {
    ($($name:ident => $path:literal,)+) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum Icon {
            $($name,)+
        }

        impl Icon {
            pub const ALL: &'static [Icon] = &[$(Icon::$name,)+];

            /// SVG path commands.
            pub fn path(self) -> &'static str {
                match self {
                    $(Icon::$name => $path,)+
                }
            }
        }
    };
}

// A dot is a tiny circle; the round stroke fills it in.
icons! {
    // Window chrome.
    Close => "M6 6 L14 14 M14 6 L6 14",
    Plus => "M10 4.5 V15.5 M4.5 10 H15.5",
    Minus => "M4.5 10 H15.5",
    ChevronDown => "M6 8 L10 12 L14 8",
    ChevronUp => "M6 12 L10 8 L14 12",
    ChevronRight => "M8 6 L12 10 L8 14",
    Sidebar => "M3.5 4.5 H16.5 V15.5 H3.5 Z M8 4.5 V15.5",
    Sun => "M7 10 a3 3 0 1 0 6 0 a3 3 0 1 0 -6 0 M10 2.5 V4.5 M10 15.5 V17.5 M2.5 10 H4.5 M15.5 10 H17.5 M4.7 4.7 L6.1 6.1 M13.9 13.9 L15.3 15.3 M4.7 15.3 L6.1 13.9 M13.9 6.1 L15.3 4.7",
    Moon => "M15.5 11.8 A6.5 6.5 0 1 1 8.2 4.5 A5 5 0 0 0 15.5 11.8 Z",
    FolderOpen => "M3.5 15.5 V5 H8 L9.5 7 H16.5 V9.5 M3.5 15.5 L5.5 9.5 H17.5 L15.5 15.5 Z",
    Folder => "M3.5 5 H8 L9.5 7 H16.5 V15.5 H3.5 Z",
    Refresh => "M15.5 10 A5.5 5.5 0 1 1 13.9 6.1 M14.3 2.8 L13.9 6.1 L10.6 5.7",
    Clock => "M3.5 10 a6.5 6.5 0 1 0 13 0 a6.5 6.5 0 1 0 -13 0 M10 6.5 V10 L12.5 11.5",
    Search => "M3.5 8.5 a5 5 0 1 0 10 0 a5 5 0 1 0 -10 0 M12.2 12.2 L16.5 16.5",
    Globe => "M3.5 10 a6.5 6.5 0 1 0 13 0 a6.5 6.5 0 1 0 -13 0 M3.5 10 H16.5 M10 3.5 C7.2 6 7.2 14 10 16.5 M10 3.5 C12.8 6 12.8 14 10 16.5",
    Assistant => "M10 3 L11.7 8.3 L17 10 L11.7 11.7 L10 17 L8.3 11.7 L3 10 L8.3 8.3 Z",
    Check => "M4.5 10.5 L8.5 14 L15.5 6",
    More => "M4.6 10 a0.4 0.4 0 1 0 0.8 0 a0.4 0.4 0 1 0 -0.8 0 M9.6 10 a0.4 0.4 0 1 0 0.8 0 a0.4 0.4 0 1 0 -0.8 0 M14.6 10 a0.4 0.4 0 1 0 0.8 0 a0.4 0.4 0 1 0 -0.8 0",

    // Shared editing.
    Undo => "M7 5 L3.5 8.5 L7 12 M3.5 8.5 H12 A4.5 4.5 0 0 1 12 17.5 H8.5",
    Redo => "M13 5 L16.5 8.5 L13 12 M16.5 8.5 H8 A4.5 4.5 0 0 0 8 17.5 H11.5",
    Print => "M6 7.5 V3.5 H14 V7.5 M6 13.5 H3.5 V7.5 H16.5 V13.5 H14 M6 11.5 H14 V16.5 H6 Z",
    Link => "M8.6 11.4 a3 3 0 0 0 4.2 0 l2.2 -2.2 a3 3 0 0 0 -4.2 -4.2 l-1 1 M11.4 8.6 a3 3 0 0 0 -4.2 0 l-2.2 2.2 a3 3 0 0 0 4.2 4.2 l1 -1",
    Image => "M3.5 4.5 H16.5 V15.5 H3.5 Z M3.5 13 L7.5 9.5 L11 12.5 L13.5 10.5 L16.5 13 M12.6 7.5 a0.4 0.4 0 1 0 0.8 0 a0.4 0.4 0 1 0 -0.8 0",
    Table => "M3.5 4.5 H16.5 V15.5 H3.5 Z M3.5 8.5 H16.5 M8 4.5 V15.5",
    Comment => "M4 4.5 H16 V13 H9.5 L6.5 16 V13 H4 Z",
    Highlight => "M12 3.5 L16.5 8 L9.5 15 H5 V10.5 Z M3.5 17.5 H16.5",
    ClearFormat => "M5 4.5 H15 M10 4.5 V15.5 M13.5 12.5 L17 16 M17 12.5 L13.5 16",

    // Paragraphs.
    AlignLeft => "M3.5 5 H16.5 M3.5 8.5 H11.5 M3.5 11.5 H16.5 M3.5 15 H11.5",
    AlignCenter => "M3.5 5 H16.5 M6 8.5 H14 M3.5 11.5 H16.5 M6 15 H14",
    AlignRight => "M3.5 5 H16.5 M8.5 8.5 H16.5 M3.5 11.5 H16.5 M8.5 15 H16.5",
    AlignJustify => "M3.5 5 H16.5 M3.5 8.5 H16.5 M3.5 11.5 H16.5 M3.5 15 H16.5",
    BulletList => "M8 5.5 H16.5 M8 10 H16.5 M8 14.5 H16.5 M4.1 5.5 a0.4 0.4 0 1 0 0.8 0 a0.4 0.4 0 1 0 -0.8 0 M4.1 10 a0.4 0.4 0 1 0 0.8 0 a0.4 0.4 0 1 0 -0.8 0 M4.1 14.5 a0.4 0.4 0 1 0 0.8 0 a0.4 0.4 0 1 0 -0.8 0",
    NumberedList => "M8.5 5.5 H16.5 M8.5 10 H16.5 M8.5 14.5 H16.5 M3.8 4.2 L5 3.6 V7.4 M3.6 12.6 H5.8 L3.6 16.4 H5.9",
    Checklist => "M3.5 5.5 L5 7 L7.5 4 M10 5.5 H16.5 M3.5 12.5 L5 14 L7.5 11 M10 12.5 H16.5",
    LineSpacing => "M9.5 5 H16.5 M9.5 10 H16.5 M9.5 15 H16.5 M5 4.5 V15.5 M3.3 6.2 L5 4.5 L6.7 6.2 M3.3 13.8 L5 15.5 L6.7 13.8",
    Quote => "M4 5 V15 M8 6.5 H16 M8 10 H16 M8 13.5 H13",
    Code => "M7.5 6 L3.5 10 L7.5 14 M12.5 6 L16.5 10 L12.5 14",

    // Sheets.
    Sum => "M15 5 H5.5 L10.5 10 L5.5 15 H15",
    Filter => "M3.5 5 H16.5 L11.5 11 V15.5 L8.5 14 V11 Z",
    Sort => "M6 4.5 V15.5 M3.5 13 L6 15.5 L8.5 13 M14 15.5 V4.5 M11.5 7 L14 4.5 L16.5 7",
    Borders => "M3.5 3.5 H16.5 V16.5 H3.5 Z M10 3.5 V16.5 M3.5 10 H16.5",
    MergeCells => "M3.5 5 H16.5 V15 H3.5 Z M6.5 10 H13.5 M11.5 8 L13.5 10 L11.5 12 M8.5 8 L6.5 10 L8.5 12",
    FillColor => "M9 3.5 L15 9.5 L9.5 15 L4 9.5 Z M4.6 9.5 H14.4 M16.5 12.5 V15",
    WrapText => "M3.5 5 H16.5 M3.5 10 H14 a2.5 2.5 0 0 1 0 5 H11 M12.5 13.5 L11 15 L12.5 16.5 M3.5 15 H7.5",
    Chart => "M4 4 V16 H16 M7.5 13 V9.5 M10.5 13 V6.5 M13.5 13 V11",

    // Slides.
    Pointer => "M5 3.5 V15 L8.2 12.2 L10.4 17 L12.4 16 L10.2 11.4 L14.5 11.2 Z",
    TextBox => "M3.5 4.5 H16.5 V15.5 H3.5 Z M7 8 H13 M10 8 V12.5",
    Shapes => "M3.5 9 H11 V16.5 H3.5 Z M9.5 7 a3.5 3.5 0 1 0 7 0 a3.5 3.5 0 1 0 -7 0",
    NewSlide => "M3.5 5 H16.5 V15 H3.5 Z M10 7.5 V12.5 M7.5 10 H12.5",
    Layout => "M3.5 4.5 H16.5 V15.5 H3.5 Z M3.5 8 H16.5 M9 8 V15.5",
    Play => "M6.5 4.5 L15.5 10 L6.5 15.5 Z",

    // Markdown and PDF.
    SplitView => "M3.5 4.5 H16.5 V15.5 H3.5 Z M10 4.5 V15.5",
    Eye => "M2.5 10 C5 5.5 15 5.5 17.5 10 C15 14.5 5 14.5 2.5 10 Z M8 10 a2 2 0 1 0 4 0 a2 2 0 1 0 -4 0",
    Pen => "M4 16 L5 12 L13.5 3.5 L16.5 6.5 L8 15 Z",
    Thumbnails => "M3.5 4.5 H8.5 V9 H3.5 Z M3.5 11.5 H8.5 V16 H3.5 Z M11 5.5 H16.5 M11 8 H15 M11 12.5 H16.5 M11 15 H15",
    FitWidth => "M3.5 4.5 H16.5 V15.5 H3.5 Z M6.5 10 H13.5 M8 8.5 L6.5 10 L8 11.5 M12 8.5 L13.5 10 L12 11.5",
    Rotate => "M15.5 10 a5.5 5.5 0 1 1 -1.8 -4.1 M15.5 3.5 V6.5 H12.5",
}

#[cfg(test)]
mod tests {
    use super::*;

    /// How many numbers each path command takes.
    fn arity(command: char) -> usize {
        match command.to_ascii_uppercase() {
            'M' | 'L' => 2,
            'H' | 'V' => 1,
            'C' => 6,
            'A' => 7,
            'Z' => 0,
            other => panic!("path command {other:?} is not one the icons may use"),
        }
    }

    /// Split path data into commands with their numbers, checking the syntax:
    /// a command letter, directly followed by its first number, the rest
    /// separated by spaces.
    fn segments(icon: Icon) -> Vec<(char, Vec<f32>)> {
        let mut segments: Vec<(char, Vec<f32>)> = Vec::new();
        for word in icon.path().split(' ') {
            let first = word
                .chars()
                .next()
                .unwrap_or_else(|| panic!("{icon:?}: double space"));
            let number = if first.is_ascii_alphabetic() {
                segments.push((first, Vec::new()));
                &word[1..]
            } else {
                word
            };
            if number.is_empty() {
                continue;
            }
            let value: f32 = number
                .parse()
                .unwrap_or_else(|_| panic!("{icon:?}: {number:?} is not a number"));
            segments
                .last_mut()
                .unwrap_or_else(|| panic!("{icon:?}: number before any command"))
                .1
                .push(value);
        }
        for (command, numbers) in &segments {
            assert_eq!(
                numbers.len(),
                arity(*command),
                "{icon:?}: wrong number count for {command}"
            );
        }
        segments
    }

    #[test]
    fn every_path_is_well_formed() {
        for icon in Icon::ALL {
            let segments = segments(*icon);
            assert_eq!(segments[0].0, 'M', "{icon:?} must start with a move");
        }
    }

    #[test]
    fn absolute_coordinates_stay_on_the_grid() {
        for icon in Icon::ALL {
            for (command, numbers) in segments(*icon) {
                // Lower case is relative, and the first five arc numbers are not points.
                let points: &[f32] = match command {
                    'M' | 'L' | 'H' | 'V' | 'C' => &numbers,
                    'A' => &numbers[5..],
                    _ => &[],
                };
                for value in points {
                    assert!(
                        (1.0..=GRID - 1.0).contains(value),
                        "{icon:?}: {value} leaves no room for the stroke"
                    );
                }
            }
        }
    }

    #[test]
    fn no_two_icons_share_a_drawing() {
        for (index, a) in Icon::ALL.iter().enumerate() {
            for b in &Icon::ALL[index + 1..] {
                assert_ne!(a.path(), b.path(), "{a:?} and {b:?} look the same");
            }
        }
    }
}
