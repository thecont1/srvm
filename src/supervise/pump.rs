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
        let reader = BufReader::new(reader);

        for line in reader.lines().map_while(Result::ok) {
            let clean = scan::strip_ansi(&line);
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
                    println!("    {line}");
                } else if let Some(line) = collapse.accept(&line) {
                    println!("    {line}");
                }
            }
        }

        if !options.quiet
            && !options.verbose
            && let Some(line) = collapse.flush()
        {
            println!("    {line}");
        }
    })
}
