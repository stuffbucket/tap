//! The terminal state vt.rs reads: what an excision must carry across its
//! splice, and the search that finds text on the screen.

use tap_index::vt::{CursorShape, Found, Scrollback, StateExtra, Terminal};

fn term(bytes: &[u8]) -> Terminal {
    let mut term = Terminal::new(20, 5, Scrollback::Unlimited).unwrap();
    term.write(bytes);
    term
}

#[test]
fn reads_the_state_programs_set() {
    let fresh = term(b"");
    let t = term(
        b"\x1b[?1h\x1b]4;1;rgb:12/34/56\x07\x1b]11;rgb:01/02/03\x07\x1b[=5;1u\x1b]2;title\x07\
          \x1b]7;file://h/tmp\x07\x1b)0\x1b[2;4r\x1b[4 q\x1b[1;31m\x1b]22;pointer\x07",
    );
    let set = |t: &Terminal, n: u16| t.modes().iter().any(|m| m.number == n && !m.ansi && m.set);
    assert!(!set(&fresh, 1) && set(&t, 1));
    assert_eq!(t.palette()[1], [0x12, 0x34, 0x56]);
    assert_eq!(fresh.palette()[1], fresh.default_palette()[1]);
    assert_eq!(t.dynamic_colors().background, Some([1, 2, 3]));
    assert_eq!(fresh.dynamic_colors().background, None);
    assert_eq!(t.kitty_keyboard_flags(), 5);
    assert_eq!(t.title_bytes(), b"title");
    assert_eq!(t.pwd(), b"file://h/tmp");
    assert_eq!(t.mouse_shape(), "pointer");
    assert_eq!(t.cursor_shape(), (CursorShape::Underline, false));
    assert_eq!(t.state_extra(StateExtra::Charsets), b"\x1b)0");
    assert_eq!(t.state_extra(StateExtra::ScrollingRegion), b"\x1b[2;4r");
    assert_eq!(fresh.state_extra(StateExtra::Charsets), b"");
    assert_eq!(fresh.state_extra(StateExtra::ScrollingRegion), b"");
    assert_ne!(
        t.state_extra(StateExtra::Style),
        fresh.state_extra(StateExtra::Style)
    );
}

#[test]
fn finds_text_in_screen_coordinates() {
    let mut t = term(b"one\r\ntwo hello\r\n\r\n\r\n\r\n\r\nHELLO");
    let mut found = t.find(b"hello");
    found.sort_by_key(|f| f.start.1);
    // The first match has scrolled into history; ASCII case is ignored.
    assert_eq!(
        found,
        vec![
            Found {
                start: (4, 1),
                end: (8, 1)
            },
            Found {
                start: (0, 6),
                end: (4, 6)
            }
        ]
    );
    assert_eq!(t.text_between((4, 1), (8, 1)), "hello");
    assert_eq!(t.history_rows(), 2);
    let pin = t.pin(4, 1).unwrap();
    t.write(b"\r\nmore\r\n");
    assert_eq!(t.pin_position(&pin), Some((4, 1)));
}
