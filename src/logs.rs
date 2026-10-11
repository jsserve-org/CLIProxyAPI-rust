//! Bounded in-memory copy of the process log, served by `/v0/management/logs`.

use std::{
    collections::VecDeque,
    io,
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

use tracing_subscriber::fmt::MakeWriter;

const MAX_LINES: usize = 2000;

static LINES: Mutex<VecDeque<(i64, String)>> = Mutex::new(VecDeque::new());

/// `tracing` writer that appends every formatted event to the buffer.
#[derive(Clone, Copy)]
pub struct BufferWriter;

impl io::Write for BufferWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        let mut lines = LINES.lock().unwrap();
        for line in String::from_utf8_lossy(bytes).lines() {
            if lines.len() == MAX_LINES {
                lines.pop_front();
            }
            lines.push_back((now, line.to_owned()));
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl MakeWriter<'_> for BufferWriter {
    type Writer = Self;

    fn make_writer(&self) -> Self {
        *self
    }
}

/// Lines logged after the unix second `after`, plus the newest timestamp held.
pub fn since(after: i64) -> (Vec<String>, i64) {
    let lines = LINES.lock().unwrap();
    let latest = lines.back().map_or(after, |(timestamp, _)| *timestamp);
    let selected = lines
        .iter()
        .filter(|(timestamp, _)| *timestamp > after)
        .map(|(_, line)| line.clone())
        .collect();
    (selected, latest)
}

pub fn clear() {
    LINES.lock().unwrap().clear();
}
