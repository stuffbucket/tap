//! A long synthetic shell recording, for measuring checkpoints.
//!
//! Shared by `tests/checkpoint_memory.rs` and `examples/checkpoint_bench.rs`
//! (through `#[path]`). A session is a run of shell commands, each with an
//! OSC 133 prompt, typed keys echoed back, output and an exit status, then a
//! pause. The output is a mix of what a long session prints: styled build
//! logs with lines long enough to wrap, fast plain log dumps, full-screen
//! programs redrawing the alternate screen, `\r` progress bars, hyperlinked
//! file listings and short replies. The pane is resized now and then.
//! Output is cut into events at arbitrary bytes, so characters and escape
//! sequences are split between events as PTY reads split them.
//!
//! Everything is derived from the seed, so a session reproduces exactly.

use base64::Engine as _;
use tap_model::Session;

/// Deterministic pseudo-random numbers (xorshift64*).
pub struct Rng(pub u64);

impl Rng {
    /// The next number.
    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    /// A number in `0..n`.
    pub fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }

    /// A number in `lo..=hi`.
    pub fn range(&mut self, lo: u64, hi: u64) -> u64 {
        lo + self.below(hi - lo + 1)
    }
}

struct Recorder {
    rng: Rng,
    events: Vec<serde_json::Value>,
    at: u64,
    bytes: usize,
    cols: u64,
    rows: u64,
}

impl Recorder {
    fn push(&mut self, mut value: serde_json::Value) {
        value["atUs"] = self.at.into();
        self.events.push(value);
    }

    /// Output spread evenly over `duration_us`, cut into events of 256 B to
    /// 16 KiB at arbitrary bytes.
    fn output(&mut self, bytes: &[u8], duration_us: u64) {
        let mut cuts = Vec::new();
        let mut at = 0;
        while at < bytes.len() {
            let n = self.rng.range(256, 16 * 1024) as usize;
            at = (at + n).min(bytes.len());
            cuts.push(at);
        }
        let step = duration_us / cuts.len().max(1) as u64;
        let mut from = 0;
        for to in cuts {
            let chunk = base64::engine::general_purpose::STANDARD.encode(&bytes[from..to]);
            self.push(serde_json::json!({"type": "output", "data": chunk, "encoding": "base64"}));
            self.bytes += to - from;
            from = to;
            self.at += step;
        }
    }

    fn key(&mut self, data: &str) {
        self.push(serde_json::json!({"type": "input", "data": data, "origin": "keyboard"}));
    }

    fn resize(&mut self, cols: u64, rows: u64) {
        self.cols = cols;
        self.rows = rows;
        self.push(serde_json::json!({"type": "resize", "cols": cols, "rows": rows}));
    }

    fn wait(&mut self, us: u64) {
        self.at += us;
    }

    /// Waits a random time in `lo..=hi` microseconds.
    fn pause(&mut self, lo: u64, hi: u64) {
        let us = self.rng.range(lo, hi);
        self.wait(us);
    }

    /// A prompt, the command typed and echoed key by key, and Enter.
    fn command_line(&mut self, command: &str) {
        self.output(
            b"\x1b]133;A\x07\x1b[1;32mme@host\x1b[0m:\x1b[1;34m~/src/tap\x1b[0m$ \x1b]133;B\x07",
            1_000,
        );
        self.pause(500_000, 4_000_000);
        for c in command.chars() {
            self.key(&c.to_string());
            self.wait(2_000);
            self.output(c.to_string().as_bytes(), 0);
            self.pause(40_000, 160_000);
        }
        self.key("\r");
        self.wait(3_000);
        self.output(b"\r\n\x1b]133;C\x07", 0);
    }

    fn command_end(&mut self, status: u64) {
        self.output(format!("\x1b]133;D;{status}\x07").as_bytes(), 0);
    }

    /// A compiler's log: styled status words, paths and warnings long
    /// enough to wrap.
    fn build_log(&mut self) {
        let lines = self.rng.range(100, 1_500);
        let mut out = Vec::new();
        for i in 0..lines {
            let line = match self.rng.below(10) {
                0 => format!(
                    "\x1b[1;33mwarning\x1b[0m\x1b[1m: unused variable `value_{i}` in function `handle_request_{i}`, consider prefixing it with an underscore: `_value_{i}` (src/server/handlers/request_{i}.rs:{}:{})\x1b[0m\r\n",
                    i % 400,
                    i % 80
                ),
                1 => format!(
                    "\x1b[38;5;{}m    Finished\x1b[0m target {i} in {}.{:02}s\r\n",
                    i % 230 + 16,
                    i % 60,
                    i % 100
                ),
                _ => format!(
                    "\x1b[1;32m   Compiling\x1b[0m crate-{i} v0.{}.{} (/home/me/src/tap/crates/crate-{i})\r\n",
                    i % 17,
                    i % 9
                ),
            };
            out.extend_from_slice(line.as_bytes());
        }
        let duration = lines * self.rng.range(2_000, 12_000);
        self.output(&out, duration);
    }

    /// A fast dump of plain log lines.
    fn log_dump(&mut self) {
        let lines = self.rng.range(500, 6_000);
        let mut out = Vec::new();
        for i in 0..lines {
            out.extend_from_slice(
                format!(
                    "2026-11-26T12:{:02}:{:02}.{:03}Z INFO request id={:016x} path=/api/v1/items/{} status=200 dur={}ms\r\n",
                    i / 60 % 60,
                    i % 60,
                    i % 1000,
                    self.rng.next(),
                    i % 5000,
                    i % 300
                )
                .as_bytes(),
            );
        }
        let duration = self.rng.range(500_000, 4_000_000);
        self.output(&out, duration);
    }

    /// A full-screen program: the alternate screen, redrawn in full every
    /// frame with absolute cursor positioning and colours.
    fn full_screen(&mut self) {
        self.output(b"\x1b[?1049h\x1b[?25l\x1b[2J", 0);
        let frames = self.rng.range(20, 300);
        for frame in 0..frames {
            let mut out = Vec::new();
            for row in 1..=self.rows {
                let text = format!(
                    "{frame:>6} {row:>3} cpu {:>3}% mem {:>5}M proc-{}",
                    (frame * 7 + row) % 100,
                    (frame * 13 + row * 31) % 99_999,
                    row * frame % 977
                );
                let width = self.cols as usize;
                out.extend_from_slice(
                    format!(
                        "\x1b[{row};1H\x1b[{}m{:<width$.width$}\x1b[0m",
                        if row == 1 {
                            "7".to_string()
                        } else {
                            format!("3{}", row % 7 + 1)
                        },
                        text,
                    )
                    .as_bytes(),
                );
            }
            self.output(&out, 0);
            self.pause(100_000, 1_000_000);
        }
        self.output(b"\x1b[?25h\x1b[?1049l", 0);
    }

    /// A progress bar redrawn in place with `\r`.
    fn progress(&mut self) {
        let steps = self.rng.range(50, 400);
        for step in 0..=steps {
            let done = (step * 40 / steps) as usize;
            self.output(
                format!(
                    "\r\x1b[2K\x1b[36m[{}{}]\x1b[0m {:>3}% downloading layer {:x}",
                    "#".repeat(done),
                    " ".repeat(40 - done),
                    step * 100 / steps,
                    step
                )
                .as_bytes(),
                0,
            );
            self.pause(20_000, 200_000);
        }
        self.output(b"\r\n", 0);
    }

    /// A file listing with an OSC 8 hyperlink on every name.
    fn hyperlinks(&mut self) {
        let lines = self.rng.range(50, 1_000);
        let mut out = Vec::new();
        for i in 0..lines {
            for j in 0..4 {
                out.extend_from_slice(
                    format!(
                        "\x1b]8;;file:///home/me/src/tap/dir{i}/file{j}.rs\x1b\\\x1b[34mfile{i}_{j}.rs\x1b[0m\x1b]8;;\x1b\\   "
                    )
                    .as_bytes(),
                );
            }
            out.extend_from_slice(b"\r\n");
        }
        let duration = self.rng.range(100_000, 1_000_000);
        self.output(&out, duration);
    }

    fn short_reply(&mut self) {
        let lines = self.rng.range(1, 12);
        let mut out = Vec::new();
        for i in 0..lines {
            out.extend_from_slice(format!("\x1b[32mM\x1b[0m  src/file_{i}.rs\r\n").as_bytes());
        }
        self.output(&out, 20_000);
    }
}

/// A session of at least `minutes` of source time and `min_bytes` of output.
pub fn long_session(minutes: u64, min_bytes: usize, seed: u64) -> Session {
    let mut r = Recorder {
        rng: Rng(seed),
        events: Vec::new(),
        at: 0,
        bytes: 0,
        cols: 120,
        rows: 36,
    };
    let end = minutes * 60_000_000;
    let mut commands = 0u64;
    while r.at < end || r.bytes < min_bytes {
        let kind = r.rng.below(100);
        let (command, status) = match kind {
            0..=29 => ("cargo build --workspace", 0),
            30..=44 => ("tail -n 20000 logs/server.log", 0),
            45..=54 => ("htop", 0),
            55..=64 => ("docker pull ghcr.io/example/image:latest", 0),
            65..=74 => ("ls --hyperlink=always -R", 0),
            _ => ("git status --short", 1),
        };
        r.command_line(command);
        match kind {
            0..=29 => r.build_log(),
            30..=44 => r.log_dump(),
            45..=54 => r.full_screen(),
            55..=64 => r.progress(),
            65..=74 => r.hyperlinks(),
            _ => r.short_reply(),
        }
        r.command_end(status);
        commands += 1;
        // A pause before the next prompt, and now and then a resize.
        r.pause(1_000_000, 15_000_000);
        if commands % 50 == 25 {
            let (cols, rows) = (r.rng.range(80, 200), r.rng.range(24, 60));
            r.resize(cols, rows);
        }
    }
    let project = serde_json::json!({
        "version": 1,
        "title": "long synthetic session",
        "createdAt": "2026-11-26T00:00:00.000Z",
        "durationUs": r.at,
        "canvas": {"width": 1280, "height": 720, "background": "#000000", "fps": 30},
        "panes": [{
            "id": "main", "cols": 120, "rows": 36, "fontSize": 14,
            "layout": {"x": 0, "y": 0, "width": 1, "height": 1},
            "events": r.events,
        }],
        "overlays": [],
        "metadata": {"terminal": "ghostty", "terminalEngine": "test", "timebase": "microseconds"},
    });
    tap_model::legacy::import_json(&project.to_string()).expect("a valid project")
}
