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
    workspace::Candidate,
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
    app_env: &[(String, String)],
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

    if !options.no_install && !spec.installs.is_empty() {
        run_bootstrap(root, spec, options, path_prepend, None, app_env)?;
    }

    let opened = Arc::new(AtomicBool::new(false));
    run_server(
        root,
        spec,
        requested,
        options,
        path_prepend,
        None,
        &opened,
        app_env,
        &[],
    )
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
    pub candidate: &'a Candidate,
    pub path_prepend: Vec<PathBuf>,
    /// Parsed `.env` pairs for this app, injected for unset vars only.
    pub env: Vec<(String, String)>,
}

/// Launches several apps under one supervisor: ports are allocated up front
/// so every app gets a distinct one, installs still run sequentially, and one
/// worker thread supervises each child. Any app failing shuts the rest down
/// and propagates the error; apps that exit 0 keep the others running.
pub fn run_many(items: &[LaunchItem], options: SupervisorOptions) -> Result<()> {
    if items.iter().any(|item| item.candidate.spec.is_static) {
        bail!("static serving does not participate in multi-app launch");
    }
    if items.len() < 2 {
        bail!("run_many requires at least two launch items");
    }

    install_signal_handler()?;

    let mut requested = Vec::with_capacity(items.len());
    for item in items {
        requested.push(plan_port(&item.candidate.spec, options, Some(&item.label))?);
    }

    for item in items {
        if !options.no_install && !item.candidate.spec.installs.is_empty() {
            run_bootstrap(
                &item.candidate.root,
                &item.candidate.spec,
                options,
                &item.path_prepend,
                Some(&item.label),
                &item.env,
            )?;
        }
    }

    // Hold every reservation while selecting so siblings cannot win the same
    // port, and release them only right before the workers spawn so installs
    // never widen the reservation-to-bind gap. With --port N the first app
    // starts at N and each later app starts one past the previously selected
    // port; --port 0 is OS-assigned per app.
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
    // Every port this launch selected, so a retry can steer clear of siblings.
    let sibling_ports: Vec<u16> = selected.iter().flatten().copied().collect();

    let opened = Arc::new(AtomicBool::new(false));
    thread::scope(|scope| -> Result<()> {
        let (tx, rx) = mpsc::channel();
        for (idx, item) in items.iter().enumerate() {
            let tx = tx.clone();
            let opened = opened.clone();
            let start = selected[idx];
            let siblings = sibling_ports.clone();
            scope.spawn(move || {
                let result = run_server(
                    &item.candidate.root,
                    &item.candidate.spec,
                    start,
                    options,
                    &item.path_prepend,
                    Some(item.label.as_str()),
                    &opened,
                    &item.env,
                    &siblings,
                );
                let _ = tx.send(result);
            });
        }
        drop(tx);

        let mut first_error = None;
        let mut pending = items.len();
        let mut deadline = None;
        while pending > 0 {
            // Workers still alive past the teardown deadline would make the
            // scope block on join; kill what is registered and exit instead.
            if deadline.is_some_and(|d: Instant| Instant::now() >= d) {
                eprintln!("  failed     sibling apps did not stop within 5s; forcing exit");
                terminate_registered_children();
                std::process::exit(1);
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

/// Runs every bootstrap install step in order and then records the stamp that
/// makes the next run cheap. A failing step aborts before anything is stamped,
/// so a partial bootstrap is never mistaken for a complete one.
fn run_bootstrap(
    root: &Path,
    spec: &ServeSpec,
    options: SupervisorOptions,
    path_prepend: &[PathBuf],
    label: Option<&str>,
    app_env: &[(String, String)],
) -> Result<()> {
    for install in &spec.installs {
        println!(
            "  step       {}installing dependencies — {}",
            labeled(label),
            install.command_line()
        );
        run_install(root, install, options, path_prepend, label, app_env)?;
    }
    if let Some(stamp) = &spec.stamp {
        crate::bootstrap::record(root, stamp)?;
    }
    Ok(())
}

fn run_install(
    root: &Path,
    command: &CommandSpec,
    options: SupervisorOptions,
    path_prepend: &[PathBuf],
    label: Option<&str>,
    app_env: &[(String, String)],
) -> Result<()> {
    let ring = Arc::new(Mutex::new(Ring::default()));
    let mut child = spawn(command, root, &child_env(app_env, &[]), path_prepend)?;
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

#[allow(clippy::too_many_arguments)]
fn run_server(
    root: &Path,
    spec: &ServeSpec,
    requested: Option<u16>,
    options: SupervisorOptions,
    path_prepend: &[PathBuf],
    label: Option<&str>,
    opened: &Arc<AtomicBool>,
    app_env: &[(String, String)],
    siblings: &[u16],
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
            app_env,
            siblings,
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
    app_env: &[(String, String)],
    siblings: &[u16],
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
    let mut child = spawn(&command, root, &child_env(app_env, &env), path_prepend)?;
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
            // A child that died without announcing a URL may have lost the
            // reservation-to-bind handoff, and timing cannot tell the two
            // apart: a thief can come and go before srvm looks, so an
            // occupancy probe is not evidence. Retry any such death of a
            // port-injected app within the bounded retries, moving clear of
            // the ports this launch already selected; a genuinely broken app
            // still surfaces its own output once the retries are exhausted.
            let retry = if announced.is_none() {
                selected.map(|selected| retry_start(selected, siblings))
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

/// The port to try after a handoff loss. `siblings` are the ports selected for
/// the other apps in this launch: a retry must never target one of them, since
/// those reservations are released before the children spawn, so a retried app
/// could otherwise bind a port its sibling is about to use.
fn retry_start(selected: u16, siblings: &[u16]) -> u16 {
    let highest = siblings
        .iter()
        .copied()
        .max()
        .unwrap_or(selected)
        .max(selected);
    highest.saturating_add(1)
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

/// The environment a child of an app receives: the app's `.env` pairs, then
/// srvm's own injection (a reserved port), with `BROWSER=none` forced last so a
/// dev server never hijacks the user's browser even when `.env` asks it to.
fn child_env(app_env: &[(String, String)], injected: &[(String, String)]) -> Vec<(String, String)> {
    let mut pairs: Vec<(String, String)> = app_env
        .iter()
        .filter(|(key, _)| key != "BROWSER")
        .cloned()
        .collect();
    for (key, value) in injected {
        if key == "BROWSER" {
            continue;
        }
        match pairs.iter_mut().find(|(name, _)| name == key) {
            Some(existing) => existing.1 = value.clone(),
            None => pairs.push((key.clone(), value.clone())),
        }
    }
    pairs.push(("BROWSER".into(), "none".into()));
    pairs
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
#[cfg(test)]
mod retry_tests {
    use super::retry_start;

    #[test]
    fn a_retry_moves_one_port_up_when_nothing_else_is_selected() {
        assert_eq!(retry_start(4000, &[]), 4001);
        assert_eq!(retry_start(4000, &[4000]), 4001);
    }

    #[test]
    fn a_retry_clears_every_sibling_port() {
        // The launch selected 4001 and 4002. When the first app loses 4001 it
        // must not retry onto 4002: that port belongs to its sibling.
        assert_eq!(retry_start(4001, &[4001, 4002]), 4003);
    }

    #[test]
    fn a_retry_from_the_highest_sibling_still_moves_up() {
        assert_eq!(retry_start(4002, &[4001, 4002]), 4003);
    }

    #[test]
    fn saturation_does_not_wrap_around() {
        assert_eq!(retry_start(u16::MAX, &[u16::MAX]), u16::MAX);
    }
}

#[cfg(test)]
mod env_tests {
    use super::child_env;

    fn pairs(items: &[(&str, &str)]) -> Vec<(String, String)> {
        items
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect()
    }

    #[test]
    fn reserved_ports_override_dotenv_and_browser_is_forced_last() {
        let app = pairs(&[("PORT", "9999"), ("BROWSER", "firefox"), ("DEBUG", "1")]);

        let env = child_env(&app, &pairs(&[("PORT", "54123")]));

        assert_eq!(
            env,
            vec![
                ("PORT".into(), "54123".into()),
                ("DEBUG".into(), "1".into()),
                ("BROWSER".into(), "none".into()),
            ]
        );
    }

    #[test]
    fn browser_is_injected_even_without_a_dotenv() {
        assert_eq!(child_env(&[], &[]), pairs(&[("BROWSER", "none")]));
    }
}

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
    CURRENT_CHILDREN.get_or_init(|| Arc::new(Mutex::new(Vec::new())));

    ctrlc::set_handler(move || {
        if let Some(stop) = STATIC_STOP.get()
            && !stop.swap(true, Ordering::SeqCst)
        {
            return;
        }
        SHUTDOWN.store(true, Ordering::SeqCst);
        terminate_registered_children();
        std::process::exit(130);
    })?;
    Ok(())
}

fn terminate_registered_children() {
    if let Some(children) = CURRENT_CHILDREN.get() {
        let pids = children
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .clone();
        for pid in pids {
            kill::terminate_tree_by_pid(pid);
        }
    }
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
    use super::{SupervisorOptions, announce_reported_url, probe_hint};
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
