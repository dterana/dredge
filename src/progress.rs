//! A one-line progress indicator for long scans. Counts only: it never sees a
//! path or any file content. Write errors on the target are ignored.

use std::io::Write;
use std::time::{Duration, Instant};

use crate::report::thousands;

/// Minimum time between two visible updates.
pub const MIN_INTERVAL: Duration = Duration::from_millis(200);

pub struct Progress {
    target: Box<dyn Write>,
    enabled: bool,
    min_interval: Duration,
    last_update: Option<Instant>,
    /// Width of the line currently shown, 0 when nothing is shown.
    shown: usize,
}

impl Progress {
    pub fn new(target: Box<dyn Write>, enabled: bool, min_interval: Duration) -> Progress {
        Progress {
            target,
            enabled,
            min_interval,
            last_update: None,
            shown: 0,
        }
    }

    /// A progress that never writes anything.
    pub fn disabled() -> Progress {
        Progress::new(Box::new(std::io::sink()), false, MIN_INTERVAL)
    }

    /// Reports how many files have been processed so far. Rewrites the line
    /// unless the previous update is more recent than the minimum interval.
    pub fn tick(&mut self, files_scanned: usize) {
        self.show(|| format!("dredge: scanned {} files...", thousands(files_scanned)));
    }

    /// Reports how many directory entries the walk has read so far. Same
    /// throttling as `tick`.
    pub fn walking(&mut self, entries: usize) {
        self.show(|| format!("dredge: walking... {} entries", thousands(entries)));
    }

    /// Rewrites the line, padded with spaces to cover a longer previous one.
    fn show(&mut self, line: impl FnOnce() -> String) {
        if !self.enabled {
            return;
        }
        let now = Instant::now();
        if let Some(last) = self.last_update {
            if now.duration_since(last) < self.min_interval {
                return;
            }
        }
        self.last_update = Some(now);
        let line = line();
        let width = line.len().max(self.shown);
        let pad = " ".repeat(width - line.len());
        if self.write(&format!("\r{line}{pad}")) {
            self.shown = width;
        }
    }

    /// Clears the line, if one is shown, so the report is not polluted.
    pub fn finish(&mut self) {
        if !self.enabled || self.shown == 0 {
            return;
        }
        let clear = format!("\r{}\r", " ".repeat(self.shown));
        self.shown = 0;
        self.write(&clear);
    }

    /// Writes and flushes. A failure disables further output; it never
    /// propagates.
    fn write(&mut self, s: &str) -> bool {
        let ok = self
            .target
            .write_all(s.as_bytes())
            .and_then(|_| self.target.flush())
            .is_ok();
        if !ok {
            self.enabled = false;
        }
        ok
    }
}

/// Clears the line even when a panic unwinds past the owner.
impl Drop for Progress {
    fn drop(&mut self) {
        self.finish();
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::io;
    use std::rc::Rc;

    use super::*;

    #[derive(Clone, Default)]
    struct Buf(Rc<RefCell<Vec<u8>>>);

    impl Write for Buf {
        fn write(&mut self, b: &[u8]) -> io::Result<usize> {
            self.0.borrow_mut().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl Buf {
        fn text(&self) -> String {
            String::from_utf8_lossy(&self.0.borrow()).into_owned()
        }
    }

    struct Broken;

    impl Write for Broken {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::new(io::ErrorKind::BrokenPipe, "closed"))
        }
        fn flush(&mut self) -> io::Result<()> {
            Err(io::Error::new(io::ErrorKind::BrokenPipe, "closed"))
        }
    }

    #[test]
    fn disabled_writes_nothing() {
        let buf = Buf::default();
        let mut p = Progress::new(Box::new(buf.clone()), false, Duration::ZERO);
        p.tick(1);
        p.tick(12_345);
        p.finish();
        assert_eq!(buf.text(), "");
    }

    #[test]
    fn enabled_rewrites_one_line_and_finish_clears_it() {
        let buf = Buf::default();
        let mut p = Progress::new(Box::new(buf.clone()), true, Duration::ZERO);
        p.tick(1);
        p.tick(12_345);
        let line = "dredge: scanned 12,345 files...";
        assert_eq!(buf.text(), format!("\rdredge: scanned 1 files...\r{line}"));
        p.finish();
        let text = buf.text();
        let clear = format!("\r{}\r", " ".repeat(line.len()));
        assert!(text.ends_with(&clear), "{text:?}");
    }

    #[test]
    fn finish_without_any_update_writes_nothing() {
        let buf = Buf::default();
        let mut p = Progress::new(Box::new(buf.clone()), true, Duration::ZERO);
        p.finish();
        assert_eq!(buf.text(), "");
    }

    #[test]
    fn interval_throttles_fast_ticks() {
        let buf = Buf::default();
        let mut p = Progress::new(Box::new(buf.clone()), true, Duration::from_millis(60));
        p.tick(1);
        p.tick(2);
        p.tick(3);
        assert_eq!(buf.text().matches('\r').count(), 1, "{:?}", buf.text());
        std::thread::sleep(Duration::from_millis(80));
        p.tick(4);
        let text = buf.text();
        assert_eq!(text.matches('\r').count(), 2, "{text:?}");
        assert!(text.ends_with("dredge: scanned 4 files..."), "{text:?}");
    }

    #[test]
    fn walking_shows_entries_and_a_shorter_scan_line_covers_it() {
        let buf = Buf::default();
        let mut p = Progress::new(Box::new(buf.clone()), true, Duration::ZERO);
        p.walking(7);
        p.walking(123_456);
        let walk = "dredge: walking... 123,456 entries";
        assert_eq!(
            buf.text(),
            format!("\rdredge: walking... 7 entries\r{walk}")
        );
        p.tick(5);
        let scan = "dredge: scanned 5 files...";
        let pad = " ".repeat(walk.len() - scan.len());
        assert!(
            buf.text().ends_with(&format!("\r{scan}{pad}")),
            "{:?}",
            buf.text()
        );
        p.finish();
        let clear = format!("\r{}\r", " ".repeat(walk.len()));
        assert!(buf.text().ends_with(&clear), "{:?}", buf.text());
    }

    #[test]
    fn disabled_walking_writes_nothing() {
        let buf = Buf::default();
        let mut p = Progress::new(Box::new(buf.clone()), false, Duration::ZERO);
        p.walking(1);
        p.walking(99_999);
        p.tick(3);
        p.finish();
        assert_eq!(buf.text(), "");
    }

    #[test]
    fn write_errors_are_ignored() {
        let mut p = Progress::new(Box::new(Broken), true, Duration::ZERO);
        p.tick(1);
        p.walking(2);
        p.tick(2);
        p.finish();
    }
}
