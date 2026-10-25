use std::{
    io::{BufRead, BufReader, Read},
    sync::{Arc, Mutex, mpsc::Sender},
    thread,
};

use super::{collapse::Collapser, ring::Ring, scan};

pub struct PumpOptions {
    pub verbose: bool,
    pub quiet: bool,
    pub detect_urls: bool,
    pub no_color: bool,
    pub label: Option<String>,
}

pub fn spawn_pump<R>(
    reader: R,
    ring: Arc<Mutex<Ring>>,
    url: Arc<Mutex<Option<String>>>,
    tx: Sender<String>,
    options: PumpOptions,
) -> thread::JoinHandle<()>
where
    R: Read + Send + 'static,
{
    thread::spawn(move || {
        let mut collapse = Collapser::default();
        let mut reader = BufReader::new(reader);
        let mut buf = Vec::new();

        // lines() would stop the pump at the first invalid-UTF-8 line, letting
        // the pipe fill and the child block on write. Split on bytes and
        // decode lossily instead.
        loop {
            buf.clear();
            match read_until_capped(&mut reader, b'\n', &mut buf) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            let line = String::from_utf8_lossy(&buf);
            let line = line.trim_end_matches(['\r', '\n']);
            let clean = scan::strip_ansi(line);
            ring.lock().expect("ring lock poisoned").push(clean.clone());

            if options.detect_urls
                && let Some(found) = scan::sniff_url(&clean)
            {
                let mut url = url.lock().expect("url lock poisoned");
                if url.is_none() {
                    *url = Some(found.clone());
                    let _ = tx.send(found);
                }
            }

            if !options.quiet {
                if options.verbose {
                    println!(
                        "{}",
                        format_child_line(line, options.no_color, options.label.as_deref())
                    );
                } else if let Some(line) = collapse.accept(line) {
                    println!(
                        "{}",
                        format_child_line(&line, options.no_color, options.label.as_deref())
                    );
                }
            }
        }

        if !options.quiet
            && !options.verbose
            && let Some(line) = collapse.flush()
        {
            println!(
                "{}",
                format_child_line(&line, options.no_color, options.label.as_deref())
            );
        }
    })
}

fn format_child_line(line: &str, no_color: bool, label: Option<&str>) -> String {
    let line = match label {
        // The label can carry a sub-app's relative path; sanitize it so a
        // hostile directory name cannot inject escapes into child output.
        Some(label) => format!("    [{}] {line}", scan::sanitize_display(label)),
        None => format!("    {line}"),
    };
    if no_color || std::env::var_os("NO_COLOR").is_some() {
        line
    } else {
        format!("\x1b[2m{line}\x1b[0m")
    }
}

/// Cap on a single child-output line. A child that never emits `\n` would
/// otherwise grow the pump's buffer without bound; flushing at the cap keeps
/// the line buffer O(cap) while still forwarding the bytes.
const MAX_LINE_BYTES: usize = 1024 * 1024;

/// Like `BufRead::read_until`, but the buffer is flushed once it reaches
/// `MAX_LINE_BYTES`, so an unterminated run is split into bounded chunks.
fn read_until_capped(
    reader: &mut impl BufRead,
    delim: u8,
    buf: &mut Vec<u8>,
) -> std::io::Result<usize> {
    let mut total = 0;
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Ok(total);
        }
        if let Some(pos) = available.iter().position(|&b| b == delim) {
            buf.extend_from_slice(&available[..=pos]);
            reader.consume(pos + 1);
            return Ok(total + pos + 1);
        }
        let remaining = MAX_LINE_BYTES - buf.len();
        if remaining == 0 {
            return Ok(total);
        }
        let take = available.len().min(remaining);
        buf.extend_from_slice(&available[..take]);
        reader.consume(take);
        total += take;
    }
}

#[cfg(test)]
mod tests {
    use super::format_child_line;

    #[test]
    fn child_lines_can_be_plain_or_dimmed() {
        assert_eq!(format_child_line("hello", true, None), "    hello");
        assert_eq!(
            format_child_line("hello", false, None),
            "\x1b[2m    hello\x1b[0m"
        );
        assert_eq!(
            format_child_line("hello", true, Some("django")),
            "    [django] hello"
        );
    }
}
