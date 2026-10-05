use std::{
    env,
    io::{Read, Write},
    net::{SocketAddr, TcpStream},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};

use crate::{
    detect::{CommandSpec, PortInjection, ServeSpec},
    ports, staticsrv,
};

mod collapse;
mod kill;
mod open;
mod pump;
pub mod ring;
pub mod scan;

use ring::Ring;

static CURRENT_CHILDREN: OnceLock<Arc<Mutex<Vec<u32>>>> = OnceLock::new();
static STATIC_STOP: OnceLock<Arc<AtomicBool>> = OnceLock::new();
static SHUTDOWN: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Clone, Copy)]
pub struct SupervisorOptions {
    pub no_open: bool,
    pub no_install: bool,
    pub verbose: bool,
    pub quiet: bool,
    pub no_color: bool,
    pub port: Option<u16>,
}

pub fn run(
    root: &Path,
    spec: &ServeSpec,
    options: SupervisorOptions,
    path_prepend: &[PathBuf],
) -> Result<()> {
    let stop = spec.is_static.then(|| {
        STATIC_STOP
            .get_or_init(|| Arc::new(AtomicBool::new(false)))
            .clone()
    });

    install_signal_handler()?;

    let requested = plan_port(spec, options, None)?;

    if let Some(stop) = stop {
        return run_static(root, requested, options, stop);
    }

    if !options.no_install
        && let Some(install) = &spec.install
    {
        println!(
            "  step       installing dependencies — {}",
            install.command_line()
        );
        run_install(root, install, options, path_prepend, None)?;
    }

    let opened = Arc::new(AtomicBool::new(false));
    run_server(root, spec, requested, options, path_prepend, None, &opened)
}

fn run_static(
    root: &Path,
    requested: Option<u16>,
    options: SupervisorOptions,
    stop: Arc<AtomicBool>,
) -> Result<()> {
    let start = requested.unwrap_or(8000);
    let server = staticsrv::StaticServer::bind(root, start)?;
    let actual = server.local_addr()?.port();
    report_port(start, actual, options, None);
    if !options.quiet {
        println!("  step       starting — built-in static server");
    }

    let url = format!("http://{}/", server.local_addr()?);
    let mut announced = None;
    let opened = Arc::new(AtomicBool::new(false));
    announce_reported_url(&url, Some(actual), &mut announced, &opened, options, None);

    server.serve(stop.clone())?;
    if stop.load(Ordering::SeqCst) {
        std::process::exit(130);
    }
    Ok(())
}

pub struct LaunchItem<'a> {
    pub label: String,
    pub spec: &'a ServeSpec,
    pub path_prepend: Vec<PathBuf>,
}

/// Launches several apps under one supervisor: ports are allocated up front
/// so every app gets a distinct one, installs still run sequentially, and one
/// worker thread supervises each child. Any app failing shuts the rest down
/// and propagates the error; apps that exit 0 keep the others running.
pub fn run_many(root: &Path, items: &[LaunchItem], options: SupervisorOptions) -> Result<()> {
    if items.iter().any(|item| item.spec.is_static) {
        bail!("static serving does not participate in multi-app launch");
    }
    if items.len() < 2 {
        bail!("run_many requires at least two launch items");
    }

    install_signal_handler()?;

    let mut requested = Vec::with_capacity(items.len());
    for item in items {
        requested.push(plan_port(item.spec, options, Some(&item.label))?);
    }

    // Hold every reservation while selecting so siblings cannot win the same
    // port. With --port N the first app starts at N and each later app starts
    // one past the previously selected port; --port 0 is OS-assigned per app.
    let mut held = Vec::with_capacity(items.len());
    let mut selected = Vec::with_capacity(items.len());
    let mut previous = None;
    for (idx, _) in items.iter().enumerate() {
        let start = match options.port {
            Some(0) => requested[idx].map(|_| 0),
            Some(port) => {
                requested[idx].map(|_| previous.map_or(port, |prev: u16| prev.saturating_add(1)))
            }
            None => requested[idx],
        };
        let sel = match start {
            Some(start) => {
                let listener = ports::reserve(start)
                    .with_context(|| format!("could not find a free port starting at {start}"))?;
                let port = listener.local_addr()?.port();
                held.push(listener);
                Some(port)
            }
            None => None,
        };
        if sel.is_some() {
            previous = sel;
        }
        selected.push(sel);
    }
    drop(held);

    for item in items {
        if !options.no_install
            && let Some(install) = &item.spec.install
        {
            println!(
                "  step       {}installing dependencies — {}",
                labeled(Some(&item.label)),
                install.command_line()
            );
            run_install(
                root,
                install,
                options,
                &item.path_prepend,
                Some(&item.label),
            )?;
        }
    }

    let opened = Arc::new(AtomicBool::new(false));
    thread::scope(|scope| -> Result<()> {
        let (tx, rx) = mpsc::channel();
        for (idx, item) in items.iter().enumerate() {
            let tx = tx.clone();
            let opened = opened.clone();
            let start = selected[idx];
            scope.spawn(move || {
                let result = run_server(
                    root,
                    item.spec,
                    start,
                    options,
                    &item.path_prepend,
                    Some(item.label.as_str()),
                    &opened,
                );
                let _ = tx.send(result);
            });
        }
        drop(tx);

        let mut first_error = None;
        let mut pending = items.len();
        let mut deadline = None;
        while pending > 0 {
            if deadline.is_some_and(|d: Instant| Instant::now() >= d) {
                break;
            }
            match rx.recv_timeout(Duration::from_millis(50)) {
                Ok(result) => {
                    pending -= 1;
                    if let Err(err) = result
                        && first_error.is_none()
                    {
                        SHUTDOWN.store(true, Ordering::SeqCst);
                        deadline = Some(Instant::now() + Duration::from_secs(5));
                        first_error = Some(err);
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        match first_error {
            Some(err) => Err(err),
            None => Ok(()),
        }
    })
}

fn plan_port(
    spec: &ServeSpec,
    options: SupervisorOptions,
    label: Option<&str>,
) -> Result<Option<u16>> {
    let inherited = match &spec.port {
        PortInjection::Env(key) => std::env::var(key).ok(),
        _ => None,
    };
    let requested = ports::requested_port(spec, options.port, inherited.as_deref())?;

    if matches!(spec.port, PortInjection::None)
        && (options.port.is_some() || spec.url_hint.is_some())
    {
        let mut warning = format!(
            "port overrides are unsupported for {}; leaving its ports unchanged",
            label.unwrap_or(&spec.name)
        );
        if let Some(port) = options.port {
            warning.push_str(&format!("; --port {port} ignored"));
        }
        eprintln!("  warning    {warning}");
    }

    Ok(requested)
}

fn run_install(
    root: &Path,
    command: &CommandSpec,
    options: SupervisorOptions,
    path_prepend: &[PathBuf],
    label: Option<&str>,
) -> Result<()> {
    let ring = Arc::new(Mutex::new(Ring::default()));
    let mut child = spawn(command, root, &[], path_prepend)?;
    register_child(child.id());
    let (tx, _rx) = mpsc::channel::<String>();
    let mut joins = attach_pumps(&mut child, ring.clone(), false, tx, options, label)?;
    let started = Instant::now();

    loop {
        if let Some(status) = child.try_wait()? {
            unregister_child(child.id());
            join_pumps(&mut joins);
            if status.success() {
                return Ok(());
            }
            return Err(error_with_tail(
                format!("install command failed: {}", command.command_line()),
                &ring,
                label,
            ));
        }

        if started.elapsed() > Duration::from_secs(15 * 60) {
            kill::terminate_tree(&mut child);
            unregister_child(child.id());
            join_pumps(&mut joins);
            return Err(error_with_tail(
                format!("install command timed out: {}", command.command_line()),
                &ring,
                label,
            ));
        }

        thread::sleep(Duration::from_millis(100));
    }
}

/// How many times to relaunch when a port was injected but the child dies
/// before announcing — the reservation is released before spawn, so another
/// process can claim the port in the gap and the child fails on bind.
const HANDOFF_RETRIES: usize = 2;

enum Attempt {
    Done,
    Stopped,
    Retry { next_start: u16, err: anyhow::Error },
}

fn run_server(
    root: &Path,
    spec: &ServeSpec,
    requested: Option<u16>,
    options: SupervisorOptions,
    path_prepend: &[PathBuf],
    label: Option<&str>,
    opened: &Arc<AtomicBool>,
) -> Result<()> {
    let mut start = requested;
    for attempt in 0..=HANDOFF_RETRIES {
        match serve_attempt(
            root,
            spec,
            requested,
            start,
            options,
            path_prepend,
            label,
            opened,
        )? {
            Attempt::Done | Attempt::Stopped => return Ok(()),
            Attempt::Retry { next_start, .. } if attempt < HANDOFF_RETRIES => {
                println!(
                    "  step       {}port was claimed before the app bound it; retrying",
                    labeled(label)
                );
                start = Some(next_start);
            }
            Attempt::Retry { err, .. } => return Err(err),
        }
    }
    unreachable!()
}

#[allow(clippy::too_many_arguments)]
fn serve_attempt(
    root: &Path,
    spec: &ServeSpec,
    report_start: Option<u16>,
    start: Option<u16>,
    options: SupervisorOptions,
    path_prepend: &[PathBuf],
    label: Option<&str>,
    opened: &Arc<AtomicBool>,
) -> Result<Attempt> {
    let ring = Arc::new(Mutex::new(Ring::default()));

    let mut reservation = None;
    let (command, env, selected) = match start {
        Some(start) => {
            let listener = ports::reserve(start)
                .with_context(|| format!("could not find a free port starting at {start}"))?;
            let selected = listener.local_addr()?.port();
            reservation = Some(listener);
            let (command, env) = ports::apply(spec, selected);
            (command, env, Some(selected))
        }
        None => (spec.command.clone(), Vec::new(), None),
    };

    if let (Some(start), Some(selected)) = (report_start, selected) {
        report_port(start, selected, options, label);
    }

    println!(
        "  step       {}starting — {}",
        labeled(label),
        command.command_line()
    );
    drop(reservation);
    let mut child = spawn(&command, root, &env, path_prepend)?;
    register_child(child.id());

    let (tx, rx) = mpsc::channel::<String>();
    let mut joins = attach_pumps(&mut child, ring.clone(), true, tx, options, label)?;
    let started = Instant::now();
    let probe_port = selected.or(spec.url_hint);
    let mut announced: Option<String> = None;
    let mut probed_hint = false;

    loop {
        while let Ok(url) = rx.try_recv() {
            announce_reported_url(&url, selected, &mut announced, opened, options, label);
        }

        if announced.is_none()
            && !probed_hint
            && started.elapsed() >= Duration::from_secs(12)
            && let Some(port) = probe_port
        {
            probed_hint = true;
            if let Some(url) = probe_hint(port) {
                announce_reported_url(&url, selected, &mut announced, opened, options, label);
            }
        }

        if SHUTDOWN.load(Ordering::SeqCst) {
            kill::terminate_tree(&mut child);
            unregister_child(child.id());
            join_pumps(&mut joins);
            return Ok(Attempt::Stopped);
        }

        if let Some(status) = child.try_wait()? {
            unregister_child(child.id());
            join_pumps(&mut joins);
            while let Ok(url) = rx.try_recv() {
                announce_reported_url(&url, selected, &mut announced, opened, options, label);
            }
            if status.success() {
                if announced.is_some() {
                    println!("  exited     {}{status}", labeled(label));
                }
                return Ok(Attempt::Done);
            }

            let err = if started.elapsed() < Duration::from_secs(3) {
                error_with_tail(
                    format!("{} failed early: {status}", spec.name),
                    &ring,
                    label,
                )
            } else {
                error_with_tail(format!("{} exited: {status}", spec.name), &ring, label)
            };
            // A child that died without announcing may have lost the
            // reservation-to-bind handoff. The thief can come and go between
            // the child's death and an occupancy probe, so treat a quick
            // death as evidence on its own and probe the port only for
            // slower deaths. Unrelated failures still surface unchanged once
            // the bounded retries are exhausted.
            let quick = started.elapsed() < Duration::from_secs(3);
            let retry = if announced.is_none() && quick {
                selected.map(|selected| selected.saturating_add(1))
            } else if announced.is_none() {
                selected.and_then(|selected| {
                    retry_handoff_start(selected, |port| {
                        std::net::TcpListener::bind(("127.0.0.1", port)).map(|_| ())
                    })
                })
            } else {
                None
            };
            if let Some(next_start) = retry {
                return Ok(Attempt::Retry { next_start, err });
            }
            return Err(err);
        }

        thread::sleep(Duration::from_millis(100));
    }
}

/// Occupancy probe for slower deaths: if the injected port is still held by
/// another process when the child died, relaunch one port up. Non-AddrInUse
/// bind errors (permissions, protocol issues) are not a snipe. Quick deaths
/// retry without this probe — the thief may already have come and gone.
fn retry_handoff_start(
    selected: u16,
    bind: impl FnOnce(u16) -> std::io::Result<()>,
) -> Option<u16> {
    match bind(selected) {
        Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => {
            Some(selected.saturating_add(1))
        }
        _ => None,
    }
}

fn attach_pumps(
    child: &mut std::process::Child,
    ring: Arc<Mutex<Ring>>,
    detect_urls: bool,
    tx: mpsc::Sender<String>,
    options: SupervisorOptions,
    label: Option<&str>,
) -> Result<Vec<thread::JoinHandle<()>>> {
    let url = Arc::new(Mutex::new(None));
    let label = label.map(str::to_string);
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
                label: label.clone(),
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
                label,
            },
        ));
    }

    Ok(joins)
}

fn spawn(
    command: &CommandSpec,
    root: &Path,
    env_pairs: &[(String, String)],
    path_prepend: &[PathBuf],
) -> Result<std::process::Child> {
    let mut cmd = Command::new(resolve_program(&command.program, root, path_prepend));
    cmd.args(&command.args)
        .current_dir(root)
        .env("BROWSER", "none")
        .envs(env_pairs.iter().map(|(key, value)| (key, value)))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if !path_prepend.is_empty() {
        let mut parts = path_prepend.to_vec();
        if let Some(existing) = env::var_os("PATH") {
            parts.extend(env::split_paths(&existing));
        }
        cmd.env("PATH", env::join_paths(parts)?);
    }
    kill::configure_process_group(&mut cmd);
    cmd.spawn()
        .with_context(|| format!("failed to spawn {}", command.command_line()))
}

/// Bare program names are resolved through the child's search path —
/// `path_prepend` dirs first, then PATH, shims, and node_modules/.bin — so a
/// tool detection found is also the tool spawned. On Windows this yields the
/// spawnable `.cmd`/`.exe` file, which a bare name cannot resolve to.
/// Programs written with a path (`.venv/bin/python`, `./script.sh`) are left
/// untouched; `Command` resolves them against the working directory.
fn resolve_program(program: &str, root: &Path, path_prepend: &[PathBuf]) -> PathBuf {
    let path = Path::new(program);
    if path.components().count() != 1 {
        return path.to_path_buf();
    }
    crate::detect::binpath::resolve_for_spawn(program, root, path_prepend)
        .unwrap_or_else(|| path.to_path_buf())
}

fn labeled(label: Option<&str>) -> String {
    label.map(|label| format!("[{label}] ")).unwrap_or_default()
}

fn report_port(requested: u16, selected: u16, options: SupervisorOptions, label: Option<&str>) {
    if options.quiet {
        return;
    }
    let label = labeled(label);
    if requested == 0 {
        println!("  port       {label}selected {selected}");
    } else if requested != selected {
        println!("  port       {label}{requested} busy -> {selected}");
    } else {
        println!("  port       {label}{selected}");
    }
}

fn announce_reported_url(
    url: &str,
    selected: Option<u16>,
    announced: &mut Option<String>,
    opened: &Arc<AtomicBool>,
    options: SupervisorOptions,
    label: Option<&str>,
) {
    if announced.as_deref() == Some(url) {
        return;
    }
    *announced = Some(url.to_string());

    println!("  app        {}{url}", labeled(label));
    if !opened.swap(true, Ordering::SeqCst) {
        if !options.no_open {
            let _ = open::open_browser(url);
        }
        println!("  ctrl-c to stop");
    }
    if let Some(expected) = selected
        && let Some(actual) = ports::url_port(url)
        && actual != expected
    {
        eprintln!("  port       requested {expected}, app reports {url}; override ignored");
    }
}

fn probe_hint(port: u16) -> Option<String> {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_millis(250)).ok()?;
    stream
        .set_read_timeout(Some(Duration::from_millis(250)))
        .ok()?;
    stream
        .set_write_timeout(Some(Duration::from_millis(250)))
        .ok()?;
    stream
        .write_all(
            format!("HEAD / HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n")
                .as_bytes(),
        )
        .ok()?;

    let deadline = Instant::now() + Duration::from_millis(250);
    let mut buf = Vec::with_capacity(256);
    let mut chunk = [0u8; 256];
    let end = loop {
        if let Some(end) = buf.iter().position(|byte| *byte == b'\n') {
            break end;
        }
        if buf.len() == 256 {
            return None;
        }
        let remaining = deadline.checked_duration_since(Instant::now())?;
        if remaining.is_zero() {
            return None;
        }
        stream.set_read_timeout(Some(remaining)).ok()?;
        let limit = 256 - buf.len();
        let n = stream.read(&mut chunk[..limit]).ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
    };

    let mut status = std::str::from_utf8(&buf[..end]).ok()?.split_whitespace();
    match status.next()? {
        "HTTP/1.0" | "HTTP/1.1" => {}
        _ => return None,
    }
    let raw_code = status.next()?;
    if raw_code.len() != 3 || !raw_code.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let code: u16 = raw_code.parse().ok()?;
    (100..=599)
        .contains(&code)
        .then(|| format!("http://127.0.0.1:{port}"))
}

fn error_with_tail(message: String, ring: &Arc<Mutex<Ring>>, label: Option<&str>) -> anyhow::Error {
    eprintln!("  failed     {}{message}", labeled(label));
    for line in ring.lock().expect("ring lock poisoned").tail(12) {
        eprintln!("    {line}");
    }
    anyhow::anyhow!(message)
}

fn install_signal_handler() -> Result<()> {
    let children = CURRENT_CHILDREN
        .get_or_init(|| Arc::new(Mutex::new(Vec::new())))
        .clone();

    ctrlc::set_handler(move || {
        if let Some(stop) = STATIC_STOP.get()
            && !stop.swap(true, Ordering::SeqCst)
        {
            return;
        }
        SHUTDOWN.store(true, Ordering::SeqCst);
        let pids = children
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .clone();
        for pid in pids {
            kill::terminate_tree_by_pid(pid);
        }
        std::process::exit(130);
    })?;
    Ok(())
}

fn register_child(pid: u32) {
    if let Some(children) = CURRENT_CHILDREN.get() {
        children
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .push(pid);
    }
}

fn unregister_child(pid: u32) {
    if let Some(children) = CURRENT_CHILDREN.get() {
        children
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .retain(|registered| *registered != pid);
    }
}

fn join_pumps(joins: &mut Vec<thread::JoinHandle<()>>) {
    for join in joins.drain(..) {
        let _ = join.join();
    }
}

#[cfg(test)]
mod tests {
    use super::{SupervisorOptions, announce_reported_url, probe_hint, retry_handoff_start};
    use std::{
        io::{Read, Write},
        net::{TcpListener, TcpStream},
        thread,
        time::{Duration, Instant},
    };

    fn probe_server(
        respond: impl FnOnce(&mut TcpStream) + Send + 'static,
    ) -> (u16, thread::JoinHandle<()>) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
                respond(&mut stream);
            }
        });
        (port, handle)
    }

    fn http_response(response: &'static [u8]) -> impl FnOnce(&mut TcpStream) + Send + 'static {
        move |stream| {
            let mut buf = [0; 128];
            let _ = stream.read(&mut buf);
            let _ = stream.write_all(response);
        }
    }

    #[test]
    fn probe_hint_adopts_port_serving_http() {
        let (port, handle) = probe_server(http_response(
            b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n",
        ));

        assert_eq!(probe_hint(port), Some(format!("http://127.0.0.1:{port}")));
        handle.join().unwrap();
    }

    #[test]
    fn retries_when_the_selected_port_was_claimed() {
        let port = 5000;
        let mut calls = Vec::new();

        assert_eq!(
            retry_handoff_start(port, |candidate| {
                calls.push(candidate);
                Err(std::io::Error::new(
                    std::io::ErrorKind::AddrInUse,
                    "claimed",
                ))
            }),
            Some(port.saturating_add(1))
        );
        assert_eq!(calls, vec![port]);
    }

    #[test]
    fn does_not_retry_on_bind_errors_other_than_addr_in_use() {
        let port = 5000;
        let mut calls = Vec::new();

        assert_eq!(
            retry_handoff_start(port, |candidate| {
                calls.push(candidate);
                Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "not a snipe",
                ))
            }),
            None
        );
        assert_eq!(calls, vec![port]);
    }

    #[test]
    fn does_not_retry_when_the_selected_port_is_free() {
        let port = 5000;
        let mut calls = Vec::new();

        assert_eq!(
            retry_handoff_start(port, |candidate| {
                calls.push(candidate);
                Ok(())
            }),
            None
        );
        assert_eq!(calls, vec![port]);
    }

    #[test]
    fn probe_hint_accepts_fragmented_status_line() {
        let (port, handle) = probe_server(|stream| {
            let mut buf = [0; 128];
            let _ = stream.read(&mut buf);
            stream.write_all(b"HTTP/1.1 2").unwrap();
            thread::sleep(Duration::from_millis(30));
            stream.write_all(b"00 OK\r\n").unwrap();
        });

        assert_eq!(probe_hint(port), Some(format!("http://127.0.0.1:{port}")));
        handle.join().unwrap();
    }

    #[test]
    fn probe_hint_counts_http_errors_as_alive() {
        for status in [
            b"HTTP/1.1 404 Not Found\r\n\r\n" as &[u8],
            b"HTTP/1.0 500 Server Error\r\n\r\n",
        ] {
            let (port, handle) = probe_server(http_response(status));

            assert_eq!(probe_hint(port), Some(format!("http://127.0.0.1:{port}")));
            handle.join().unwrap();
        }
    }

    #[test]
    fn probe_hint_rejects_tcp_only_endpoint() {
        let (port, handle) = probe_server(|stream| {
            let mut buf = [0; 128];
            let _ = stream.read(&mut buf);
        });

        assert_eq!(probe_hint(port), None);
        handle.join().unwrap();
    }

    #[test]
    fn probe_hint_rejects_stalled_endpoint() {
        let (port, handle) = probe_server(|stream| {
            let mut buf = [0; 128];
            let _ = stream.read(&mut buf);
            thread::sleep(Duration::from_millis(400));
        });

        assert_eq!(probe_hint(port), None);
        handle.join().unwrap();
    }

    #[test]
    fn probe_hint_slow_drip_respects_total_deadline() {
        let (port, handle) = probe_server(|stream| {
            let mut buf = [0; 128];
            let _ = stream.read(&mut buf);
            for byte in b"HTTP/1.1 200 OK\r\n" {
                if stream.write_all(&[*byte]).is_err() {
                    return;
                }
                thread::sleep(Duration::from_millis(80));
            }
        });

        let started = Instant::now();
        assert_eq!(probe_hint(port), None);
        assert!(
            started.elapsed() < Duration::from_millis(700),
            "probe should give up at the 250ms total deadline"
        );
        handle.join().unwrap();
    }

    #[test]
    fn probe_hint_rejects_missing_newline() {
        let (port, handle) = probe_server(http_response(b"HTTP/1.1 200 OK"));

        assert_eq!(probe_hint(port), None);
        handle.join().unwrap();
    }

    #[test]
    fn probe_hint_rejects_malformed_status_codes() {
        for status in [
            b"HTTP/1.1 0200\r\n" as &[u8],
            b"HTTP/1.1 +200\r\n",
            b"HTTP/1.1 20\r\n",
        ] {
            let (port, handle) = probe_server(http_response(status));

            assert_eq!(probe_hint(port), None);
            handle.join().unwrap();
        }
    }

    #[test]
    fn probe_hint_rejects_overlong_status_line() {
        let (port, handle) = probe_server(|stream| {
            let mut buf = [0; 128];
            let _ = stream.read(&mut buf);
            let _ = stream.write_all(&[b'X'; 300]);
        });

        assert_eq!(probe_hint(port), None);
        handle.join().unwrap();
    }

    #[test]
    fn probe_hint_rejects_non_http_and_out_of_range() {
        for status in [
            b"garbage\r\n" as &[u8],
            b"HTTP/2 200\r\n\r\n",
            b"HTTP/1.1 999 Weird\r\n\r\n",
            b"HTTP/1.1 099 Nope\r\n\r\n",
        ] {
            let (port, handle) = probe_server(http_response(status));

            assert_eq!(probe_hint(port), None);
            handle.join().unwrap();
        }
    }

    #[test]
    fn probe_hint_rejects_closed_port() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);

        assert_eq!(probe_hint(port), None);
    }

    fn quiet_options() -> SupervisorOptions {
        SupervisorOptions {
            no_open: true,
            no_install: false,
            verbose: false,
            quiet: true,
            no_color: true,
            port: None,
        }
    }

    #[test]
    fn first_announcement_opens_browser_once() {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };

        let mut announced = None;
        let opened = Arc::new(AtomicBool::new(false));
        let options = quiet_options();

        announce_reported_url(
            "http://127.0.0.1:8000",
            None,
            &mut announced,
            &opened,
            options,
            None,
        );
        assert_eq!(announced.as_deref(), Some("http://127.0.0.1:8000"));
        assert!(opened.load(Ordering::SeqCst));

        announce_reported_url(
            "http://127.0.0.1:8000",
            None,
            &mut announced,
            &opened,
            options,
            None,
        );
        assert_eq!(announced.as_deref(), Some("http://127.0.0.1:8000"));
        assert!(opened.load(Ordering::SeqCst));
    }

    #[test]
    fn late_sniffed_url_replaces_probe_announcement() {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };

        let mut announced = Some("http://127.0.0.1:8123".to_string());
        let opened = Arc::new(AtomicBool::new(true));
        let options = quiet_options();

        announce_reported_url(
            "http://127.0.0.1:9123/app?x=1#f",
            Some(8123),
            &mut announced,
            &opened,
            options,
            None,
        );
        assert_eq!(
            announced.as_deref(),
            Some("http://127.0.0.1:9123/app?x=1#f")
        );
        assert!(opened.load(Ordering::SeqCst));

        announce_reported_url(
            "http://127.0.0.1:9123/app?x=1#f",
            Some(8123),
            &mut announced,
            &opened,
            options,
            None,
        );
        assert_eq!(
            announced.as_deref(),
            Some("http://127.0.0.1:9123/app?x=1#f")
        );
        assert!(opened.load(Ordering::SeqCst));
    }
}
