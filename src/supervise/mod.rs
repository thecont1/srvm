use std::{
    io::Write,
    net::{SocketAddr, TcpStream},
    path::Path,
    process::{Command, Stdio},
    sync::{Arc, Mutex, OnceLock, mpsc},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};

use crate::detect::{CommandSpec, ServeSpec};

mod collapse;
mod kill;
mod open;
mod pump;
pub mod ring;
pub mod scan;

use ring::Ring;

static CURRENT_CHILD: OnceLock<Arc<Mutex<Option<u32>>>> = OnceLock::new();

#[derive(Debug, Clone, Copy)]
pub struct SupervisorOptions {
    pub no_open: bool,
    pub no_install: bool,
    pub verbose: bool,
    pub quiet: bool,
    pub no_color: bool,
}

pub fn run(root: &Path, spec: &ServeSpec, options: SupervisorOptions) -> Result<()> {
    if spec.is_static {
        bail!("static serving is not implemented yet");
    }

    install_signal_handler();

    if !options.no_install
        && let Some(install) = &spec.install
    {
        println!(
            "  step       installing dependencies — {}",
            install.command_line()
        );
        run_install(root, install, options)?;
    }

    println!("  step       starting — {}", spec.command_line());
    run_server(root, spec, options)
}

fn run_install(root: &Path, command: &CommandSpec, options: SupervisorOptions) -> Result<()> {
    let ring = Arc::new(Mutex::new(Ring::default()));
    let mut child = spawn(command, root)?;
    set_current_child(Some(child.id()));
    let (tx, _rx) = mpsc::channel::<String>();
    let mut joins = attach_pumps(&mut child, ring.clone(), false, tx, options)?;
    let started = Instant::now();

    loop {
        if let Some(status) = child.try_wait()? {
            set_current_child(None);
            join_pumps(&mut joins);
            if status.success() {
                return Ok(());
            }
            return Err(error_with_tail(
                format!("install command failed: {}", command.command_line()),
                &ring,
            ));
        }

        if started.elapsed() > Duration::from_secs(15 * 60) {
            kill::terminate_tree(&mut child);
            set_current_child(None);
            join_pumps(&mut joins);
            return Err(error_with_tail(
                format!("install command timed out: {}", command.command_line()),
                &ring,
            ));
        }

        thread::sleep(Duration::from_millis(100));
    }
}

fn run_server(root: &Path, spec: &ServeSpec, options: SupervisorOptions) -> Result<()> {
    let ring = Arc::new(Mutex::new(Ring::default()));
    let mut child = spawn(&spec.command, root)?;
    set_current_child(Some(child.id()));

    let (tx, rx) = mpsc::channel::<String>();
    let mut joins = attach_pumps(&mut child, ring.clone(), true, tx, options)?;
    let started = Instant::now();
    let mut announced = false;
    let mut probed_hint = false;

    loop {
        if !announced {
            match rx.try_recv() {
                Ok(url) => {
                    announce_url(&url, options.no_open);
                    announced = true;
                }
                Err(mpsc::TryRecvError::Disconnected | mpsc::TryRecvError::Empty) => {}
            }
        }

        if !announced
            && !probed_hint
            && started.elapsed() >= Duration::from_secs(12)
            && let Some(port) = spec.url_hint
        {
            probed_hint = true;
            if let Some(url) = probe_hint(port) {
                announce_url(&url, options.no_open);
                announced = true;
            }
        }

        if let Some(status) = child.try_wait()? {
            set_current_child(None);
            join_pumps(&mut joins);
            if !announced && let Ok(url) = rx.try_recv() {
                announce_url(&url, options.no_open);
                announced = true;
            }
            if status.success() {
                if announced {
                    println!("  exited     {}", status);
                }
                return Ok(());
            }

            if started.elapsed() < Duration::from_secs(3) {
                return Err(error_with_tail(
                    format!("{} failed early: {status}", spec.name),
                    &ring,
                ));
            }

            return Err(error_with_tail(
                format!("{} exited: {status}", spec.name),
                &ring,
            ));
        }

        thread::sleep(Duration::from_millis(100));
    }
}

fn attach_pumps(
    child: &mut std::process::Child,
    ring: Arc<Mutex<Ring>>,
    detect_urls: bool,
    tx: mpsc::Sender<String>,
    options: SupervisorOptions,
) -> Result<Vec<thread::JoinHandle<()>>> {
    let url = Arc::new(Mutex::new(None));
    let mut joins = Vec::new();

    if let Some(stdout) = child.stdout.take() {
        joins.push(pump::spawn_pump(
            stdout,
            ring.clone(),
            url.clone(),
            tx.clone(),
            pump::PumpOptions {
                verbose: options.verbose,
                quiet: options.quiet,
                detect_urls,
                no_color: options.no_color,
            },
        ));
    }

    if let Some(stderr) = child.stderr.take() {
        joins.push(pump::spawn_pump(
            stderr,
            ring,
            url,
            tx,
            pump::PumpOptions {
                verbose: options.verbose,
                quiet: options.quiet,
                detect_urls,
                no_color: options.no_color,
            },
        ));
    }

    Ok(joins)
}

fn spawn(command: &CommandSpec, root: &Path) -> Result<std::process::Child> {
    let mut cmd = Command::new(&command.program);
    cmd.args(&command.args)
        .current_dir(root)
        .env("BROWSER", "none")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    kill::configure_process_group(&mut cmd);
    cmd.spawn()
        .with_context(|| format!("failed to spawn {}", command.command_line()))
}

fn announce_url(url: &str, no_open: bool) {
    println!("  app        {url}");
    if !no_open {
        let _ = open::open_browser(url);
    }
    println!("  ctrl-c to stop");
}

fn probe_hint(port: u16) -> Option<String> {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_millis(250)).ok()?;
    stream
        .set_read_timeout(Some(Duration::from_millis(250)))
        .ok()?;
    stream
        .write_all(b"HEAD / HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
        .ok()?;
    Some(format!("http://127.0.0.1:{port}"))
}

fn error_with_tail(message: String, ring: &Arc<Mutex<Ring>>) -> anyhow::Error {
    eprintln!("  failed     {message}");
    for line in ring.lock().expect("ring lock poisoned").tail(12) {
        eprintln!("    {line}");
    }
    anyhow::anyhow!(message)
}

fn install_signal_handler() {
    let child = CURRENT_CHILD
        .get_or_init(|| Arc::new(Mutex::new(None)))
        .clone();

    let _ = ctrlc::set_handler(move || {
        let pid = *child.lock().expect("signal child lock poisoned");
        if let Some(pid) = pid {
            kill::terminate_tree_by_pid(pid);
        }
        std::process::exit(130);
    });
}

fn set_current_child(pid: Option<u32>) {
    if let Some(slot) = CURRENT_CHILD.get() {
        *slot.lock().expect("signal child lock poisoned") = pid;
    }
}

fn join_pumps(joins: &mut Vec<thread::JoinHandle<()>>) {
    for join in joins.drain(..) {
        let _ = join.join();
    }
}

#[cfg(test)]
mod tests {
    use super::probe_hint;
    use std::{io::Read, net::TcpListener, thread};

    #[test]
    fn probe_hint_adopts_open_loopback_port() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0; 64];
            let _ = stream.read(&mut buf);
        });

        assert_eq!(probe_hint(port), Some(format!("http://127.0.0.1:{port}")));
        handle.join().unwrap();
    }
}
