//! Durable, launcher-owned HTTP services. The session process never owns the
//! child: restarting an agent must not tear down its hosted UI. A stable TCP
//! relay separates the forwarded port from the port injected into each child.
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use shared::api::{CreateForwardRequest, CreateForwardResponse, SessionForwardsResponse};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{
    net::{TcpListener, TcpStream},
    process::{Child, Command},
    task::JoinHandle,
};
use uuid::Uuid;

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct Recipe {
    pub plugin: Option<String>,
    pub session: Uuid,
    pub generation: Uuid,
    pub cwd: PathBuf,
    pub program: PathBuf,
    pub args: Vec<String>,
    pub env: std::collections::BTreeMap<String, String>,
    pub health_path: String,
    pub backend: String,
}
#[derive(Serialize, Deserialize)]
pub(crate) struct Status {
    pub generation: Uuid,
    pub port: u16,
    pub child_port: u16,
    pub url: Option<String>,
    pub error: Option<String>,
    pub ready: bool,
}
fn root() -> PathBuf {
    session_lib::paths::config_dir().join("managed-forwards")
}
fn recipe_path(root: &Path, session: Uuid) -> PathBuf {
    root.join(format!("{session}.json"))
}
fn status_path(root: &Path, session: Uuid) -> PathBuf {
    root.join(format!("{session}.status"))
}
fn atomic_json(path: &Path, value: &impl Serialize) -> Result<()> {
    use std::io::Write;
    let tmp = path.with_extension(format!("{}.tmp", Uuid::new_v4()));
    let result = (|| {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut file = opts.open(&tmp)?;
        file.write_all(&serde_json::to_vec(value)?)?;
        file.sync_all()?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(tmp);
    }
    result
}
pub(crate) fn remove(session: Uuid) -> Result<()> {
    remove_at(&root(), session, None)
}
pub(crate) fn remove_owned(session: Uuid, generation: Uuid) -> Result<()> {
    remove_at(&root(), session, Some(generation))
}
pub(crate) fn remove_plugin(name: &str) -> Result<()> {
    let root = root();
    if !root.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(&root)? {
        let path = entry?.path();
        if path.extension().is_none_or(|e| e != "json") {
            continue;
        }
        if let Some(recipe) = std::fs::read(path)
            .ok()
            .and_then(|b| serde_json::from_slice::<Recipe>(&b).ok())
        {
            if recipe.plugin.as_deref() == Some(name) {
                remove_at(&root, recipe.session, Some(recipe.generation))?;
            }
        }
    }
    Ok(())
}
pub(crate) fn log_path(session: &str) -> Option<PathBuf> {
    session
        .parse::<Uuid>()
        .ok()
        .map(|id| root().join(format!("{id}.log")))
}
fn recipe_lock(root: &Path, session: Uuid) -> Result<std::fs::File> {
    std::fs::create_dir_all(root)?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(root.join(format!("{session}.lock")))?;
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        // SAFETY: file owns this descriptor and releases the advisory lock on drop.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
    }
    Ok(file)
}
fn remove_at(root: &Path, session: Uuid, generation: Option<Uuid>) -> Result<()> {
    let _lock = recipe_lock(root, session)?;
    if let Some(generation) = generation {
        let matches = std::fs::read(recipe_path(root, session))
            .ok()
            .and_then(|b| serde_json::from_slice::<Recipe>(&b).ok())
            .is_some_and(|r| r.generation == generation);
        if !matches {
            return Ok(());
        }
    }
    match std::fs::remove_file(recipe_path(root, session)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}
fn read_status(root: &Path, session: Uuid) -> Option<Status> {
    serde_json::from_slice(&std::fs::read(status_path(root, session)).ok()?).ok()
}
fn supervisor_running(root: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        let Ok(file) = std::fs::OpenOptions::new()
            .write(true)
            .open(root.join("supervisor.lock"))
        else {
            return false;
        };
        // SAFETY: this probe never waits or steals a lock; a successful lock is
        // released with file. Only EWOULDBLOCK proves an active supervisor.
        let locked = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) != 0 };
        locked && std::io::Error::last_os_error().raw_os_error() == Some(libc::EWOULDBLOCK)
    }
    #[cfg(not(unix))]
    {
        let _ = root;
        false
    }
}
pub(crate) fn registered(session: &str, generation: Uuid) -> bool {
    session
        .parse()
        .ok()
        .and_then(|id| std::fs::read(recipe_path(&root(), id)).ok())
        .and_then(|b| serde_json::from_slice::<Recipe>(&b).ok())
        .is_some_and(|r| r.generation == generation)
}
pub(crate) async fn register(mut recipe: Recipe) -> Result<Status> {
    if !cfg!(unix) {
        bail!("managed services require Linux or macOS");
    }
    let root = root();
    std::fs::create_dir_all(&root)?;
    let lock = recipe_lock(&root, recipe.session)?;
    if let Ok(bytes) = std::fs::read(recipe_path(&root, recipe.session)) {
        if let Ok(old) = serde_json::from_slice::<Recipe>(&bytes) {
            let mut candidate = recipe.clone();
            candidate.generation = old.generation;
            if candidate == old {
                recipe = old;
            }
        }
    }
    atomic_json(&recipe_path(&root, recipe.session), &recipe)?;
    drop(lock);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(45);
    loop {
        if let Some(status) =
            read_status(&root, recipe.session).filter(|s| s.generation == recipe.generation)
        {
            if let Some(error) = &status.error {
                bail!(
                    "service is retrying: {error}; log: {}",
                    root.join(format!("{}.log", recipe.session)).display()
                );
            }
            if status.ready && status.url.is_some() && supervisor_running(&root) {
                return Ok(status);
            }
        }
        if tokio::time::Instant::now() >= deadline {
            bail!("service registration persisted but not ready; ensure the updated launcher daemon is running. Logs: {}", root.display());
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}
pub(crate) async fn run_script(
    script: &Path,
    args: Vec<String>,
    health_path: String,
) -> Result<()> {
    if !health_path.starts_with('/') || health_path.contains(['\r', '\n']) {
        bail!("health path must start with /");
    }
    let (backend, token) = crate::message::api_base()?;
    let session = crate::message::current_session_id(&reqwest::Client::new(), &backend, &token)
        .await?
        .parse()?;
    let status = register(Recipe {
        plugin: None,
        session,
        generation: Uuid::new_v4(),
        cwd: std::env::current_dir()?,
        program: script
            .canonicalize()
            .context("start script must exist and be executable")?,
        args,
        env: Default::default(),
        health_path,
        backend,
    })
    .await?;
    println!("{}", status.url.unwrap_or_default());
    Ok(())
}

/// Owns the child process group, never a PID loaded from disk. Replacements and
/// launcher shutdown kill the whole tree; dropping Child alone only kills its leader.
struct Process {
    child: Child,
    group: u32,
}
impl Drop for Process {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            // SAFETY: this child was spawned as the leader of its own process group.
            unsafe {
                libc::kill(-(self.group as i32), libc::SIGKILL);
            }
        }
        let _ = self.child.start_kill();
    }
}
fn child_command(recipe: &Recipe, port: u16) -> Command {
    let mut cmd = Command::new(&recipe.program);
    cmd.args(
        recipe
            .args
            .iter()
            .map(|a| a.replace("{port}", &port.to_string())),
    )
    .current_dir(&recipe.cwd)
    .envs(&recipe.env)
    .env("PORTAL_PORT", port.to_string())
    .env("PORT", port.to_string())
    .env("PORTAL_SESSION_ID", recipe.session.to_string())
    .stdin(Stdio::null())
    .kill_on_drop(true);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.as_std_mut().process_group(0);
    }
    cmd
}
async fn relay(listener: TcpListener, target: std::sync::Arc<std::sync::atomic::AtomicU16>) {
    let mut streams = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let Ok((mut inbound, _)) = accepted else { break };
                let port = target.load(std::sync::atomic::Ordering::Acquire);
                if port == 0 { continue; }
                streams.spawn(async move {
                    if let Ok(Ok(mut outbound)) = tokio::time::timeout(Duration::from_secs(3), TcpStream::connect(("127.0.0.1", port))).await {
                        let _ = tokio::io::copy_bidirectional(&mut inbound, &mut outbound).await;
                    }
                });
            }
            _ = streams.join_next(), if !streams.is_empty() => {}
        }
    }
}
pub(crate) struct Task(pub JoinHandle<()>);
impl Drop for Task {
    fn drop(&mut self) {
        self.0.abort();
    }
}
fn current(root: &Path, recipe: &Recipe) -> bool {
    std::fs::read(recipe_path(root, recipe.session))
        .ok()
        .and_then(|b| serde_json::from_slice::<Recipe>(&b).ok())
        .is_some_and(|r| r.generation == recipe.generation)
}
type Credentials = std::sync::Arc<dyn Fn() -> Result<(String, String)> + Send + Sync>;
async fn serve(root: PathBuf, recipe: Recipe, credentials: Credentials) -> Result<()> {
    let old = read_status(&root, recipe.session).filter(|s| s.generation == recipe.generation);
    // Keep the public relay port across daemon restarts. Never adopt an unrelated listener.
    let listener = TcpListener::bind(("127.0.0.1", old.as_ref().map_or(0, |s| s.port))).await?;
    let port = listener.local_addr()?.port();
    let target = std::sync::Arc::new(std::sync::atomic::AtomicU16::new(0));
    let _relay = Task(tokio::spawn(relay(listener, target.clone())));
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()?;
    let mut status = Status {
        generation: recipe.generation,
        port,
        child_port: 0,
        url: old.and_then(|s| s.url),
        error: None,
        ready: false,
    };
    // Persist the bound port before spawning, so a daemon restart reuses it even during startup.
    atomic_json(&status_path(&root, recipe.session), &status)?;
    let mut backoff = 1u64;
    while current(&root, &recipe) {
        let reserved = TcpListener::bind(("127.0.0.1", 0)).await?;
        let child_port = reserved.local_addr()?.port();
        drop(reserved);
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(root.join(format!("{}.log", recipe.session)))?;
        let started = tokio::time::Instant::now();
        let result: Result<()> = async {
            let child = child_command(&recipe, child_port).stdout(log.try_clone()?).stderr(log).spawn()?;
            let mut process = Process { group: child.id().context("spawned service has no PID")?, child };
            let health = format!("http://127.0.0.1:{child_port}{}", recipe.health_path);
            let deadline = started + Duration::from_secs(30);
            loop {
                if let Some(exit) = process.child.try_wait()? { bail!("start command exited {exit}; it must keep the server in the foreground"); }
                if client.get(&health).send().await.is_ok_and(|r| r.status().is_success()) { break; }
                if tokio::time::Instant::now() >= deadline { bail!("health check timed out: {health}"); }
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            target.store(child_port, std::sync::atomic::Ordering::Release);
            status.child_port = child_port;
            status.error = None;
            let (base, token) = credentials()?;
            if base != recipe.backend { bail!("configured backend changed; register the service again"); }
            if status.url.is_none() {
                if !current(&root, &recipe) { return Ok(()); }
                let response = client.post(format!("{base}/api/agent/sessions/{}/forwards", recipe.session))
                    .bearer_auth(&token).json(&CreateForwardRequest { port }).send().await?.error_for_status()?;
                status.url = Some(response.json::<CreateForwardResponse>().await?.forward.url);
            }
            status.ready = true;
            atomic_json(&status_path(&root, recipe.session), &status)?;
            let mut unhealthy = 0;
            loop {
                tokio::select! {
                    exit = process.child.wait() => { bail!("service exited: {}", exit?); }
                    _ = tokio::time::sleep(Duration::from_secs(5)) => {
                        if !current(&root, &recipe) { return Ok(()); }
                        if client.get(&health).send().await.is_ok_and(|r| r.status().is_success()) { unhealthy = 0; } else { unhealthy += 1; }
                        if unhealthy >= 3 { bail!("service failed three health checks"); }
                        // A user closing or replacing the forward wins over recovery.
                        let (current_base, token) = credentials()?;
                        if current_base != base { bail!("configured backend changed; register the service again"); }
                        if let Ok(response) = client.get(format!("{base}/api/agent/sessions/{}/forwards", recipe.session)).bearer_auth(token).send().await {
                            if response.status().as_u16() == 404 || response.status().as_u16() == 410 {
                                remove_at(&root, recipe.session, Some(recipe.generation))?;
                                return Ok(());
                            }
                            if response.status().is_success() {
                                let forwards = response.json::<SessionForwardsResponse>().await?;
                                if !forwards.forwards.iter().any(|f| f.port == port) {
                                    remove_at(&root, recipe.session, Some(recipe.generation))?;
                                    return Ok(());
                                }
                            }
                        }
                    }
                }
            }
        }.await;
        target.store(0, std::sync::atomic::Ordering::Release);
        status.ready = false;
        if let Err(err) = result {
            status.error = Some(format!("{err:#}"));
            tracing::warn!(session = %recipe.session, error = %err, "managed service restarting");
            atomic_json(&status_path(&root, recipe.session), &status)?;
        }
        if started.elapsed() >= Duration::from_secs(60) {
            backoff = 1;
        }
        tokio::time::sleep(Duration::from_secs(backoff)).await;
        backoff = (backoff * 2).min(30);
    }
    Ok(())
}

/// One daemon-owned supervisor, independent of agent and backend reconnects.
pub(crate) async fn supervise() -> Result<()> {
    let root = root();
    std::fs::create_dir_all(&root)?;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(root.join("supervisor.lock"))?;
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        // SAFETY: the descriptor stays open for the full lifetime of this supervisor.
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            bail!("another managed-service supervisor is already running");
        }
    }
    let _lock = lock;
    let mut workers: HashMap<Uuid, (Uuid, Task)> = HashMap::new();
    loop {
        let mut wanted = HashMap::new();
        for entry in std::fs::read_dir(&root)? {
            let path = entry?.path();
            if path.extension().is_none_or(|e| e != "json") {
                continue;
            }
            match std::fs::read(&path)
                .ok()
                .and_then(|b| serde_json::from_slice::<Recipe>(&b).ok())
            {
                Some(recipe) if path == recipe_path(&root, recipe.session) => {
                    wanted.insert(recipe.session, recipe);
                }
                _ => {
                    tracing::warn!(path = %path.display(), "ignoring malformed managed-service recipe")
                }
            }
        }
        workers.retain(|id, (generation, task)| {
            wanted.get(id).is_some_and(|r| r.generation == *generation) && !task.0.is_finished()
        });
        for (id, recipe) in wanted {
            workers.entry(id).or_insert_with(|| {
                let generation = recipe.generation;
                let root = root.clone();
                (generation, Task(tokio::spawn(async move {
                    if let Err(err) = serve(root.clone(), recipe, std::sync::Arc::new(crate::message::api_base)).await {
                        tracing::warn!(session = %id, error = %err, "managed service unavailable");
                        let mut status = read_status(&root, id).filter(|s| s.generation == generation).unwrap_or(Status {generation, port:0, child_port:0, url:None, error:None, ready:false});
                        status.ready = false;
                        status.error = Some(format!("{err:#}"));
                        let _ = atomic_json(&status_path(&root,id), &status);
                        tokio::time::sleep(Duration::from_secs(30)).await;
                    }
                })))
            });
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicU16, AtomicUsize, Ordering},
        Arc,
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    // Spawned only by the lifecycle test. Keeping the HTTP fixture in this
    // executable makes the test independent of Python, Node, and installed plugins.
    #[test]
    fn http_fixture() {
        if std::env::var("PORTAL_MANAGED_TEST_CHILD").as_deref() != Ok("1") {
            return;
        }
        use std::io::{Read, Write};
        let port: u16 = std::env::var("PORTAL_PORT").unwrap().parse().unwrap();
        let listener = std::net::TcpListener::bind(("127.0.0.1", port)).unwrap();
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            let mut buf = [0; 4096];
            let n = stream.read(&mut buf).unwrap();
            if String::from_utf8_lossy(&buf[..n]).starts_with("GET /crash ") {
                std::process::exit(23);
            }
            let body = port.to_string();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
        }
    }
    fn recipe(root: &Path, backend: String) -> Recipe {
        Recipe {
            plugin: None,
            session: Uuid::new_v4(),
            generation: Uuid::new_v4(),
            cwd: root.into(),
            program: std::env::current_exe().unwrap(),
            args: vec![
                "--exact".into(),
                "managed_forward::tests::http_fixture".into(),
                "--nocapture".into(),
            ],
            env: [("PORTAL_MANAGED_TEST_CHILD".into(), "1".into())].into(),
            health_path: "/".into(),
            backend,
        }
    }
    async fn ready(root: &Path, session: Uuid, previous_child: Option<u16>) -> Status {
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                if let Some(s) = read_status(root, session).filter(|s| {
                    s.ready && s.error.is_none() && Some(s.child_port) != previous_child
                }) {
                    return s;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("service did not become ready")
    }
    #[tokio::test]
    async fn crashes_and_daemon_restarts_preserve_forward_and_revocation_wins() {
        let root = tempfile::tempdir().unwrap();
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let backend = format!("http://{}", listener.local_addr().unwrap());
        let forwarded = Arc::new(AtomicU16::new(0));
        let registrations = Arc::new(AtomicUsize::new(0));
        let (forward, posts) = (forwarded.clone(), registrations.clone());
        let _api = Task(tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                let mut buf = [0; 4096];
                loop {
                    let n = stream.read(&mut buf).await.unwrap();
                    if n == 0 {
                        break;
                    }
                    request.extend_from_slice(&buf[..n]);
                    if let Some(i) = request.windows(4).position(|s| s == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&request[..i]);
                        let len = headers
                            .lines()
                            .find_map(|l| {
                                l.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .and_then(|v| v.trim().parse::<usize>().ok())
                            })
                            .unwrap_or(0);
                        if request.len() >= i + 4 + len {
                            break;
                        }
                    }
                }
                let info = |port| shared::api::ForwardInfo {
                    port,
                    url: "https://stable.example/".into(),
                    created_at: String::new(),
                    public: false,
                    listening: Some(true),
                    process: None,
                };
                let body = if request.starts_with(b"POST ") {
                    let i = request.windows(4).position(|s| s == b"\r\n\r\n").unwrap() + 4;
                    let req: CreateForwardRequest = serde_json::from_slice(&request[i..]).unwrap();
                    forward.store(req.port, Ordering::SeqCst);
                    posts.fetch_add(1, Ordering::SeqCst);
                    serde_json::to_vec(&CreateForwardResponse {
                        forward: info(req.port),
                        replaced_port: None,
                        listening: Some(true),
                        probe_error: None,
                    })
                    .unwrap()
                } else {
                    let port = forward.load(Ordering::SeqCst);
                    serde_json::to_vec(&SessionForwardsResponse {
                        forwards: if port == 0 { vec![] } else { vec![info(port)] },
                        recent_failures: vec![],
                        connections: vec![],
                        connection_limit: 512,
                    })
                    .unwrap()
                };
                stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",body.len()).as_bytes()).await.unwrap();
                stream.write_all(&body).await.unwrap();
            }
        }));
        let recipe = recipe(root.path(), backend.clone());
        atomic_json(&recipe_path(root.path(), recipe.session), &recipe).unwrap();
        let auth: Credentials = Arc::new(move || Ok((backend.clone(), "test-token".into())));
        let task = tokio::spawn(serve(root.path().into(), recipe.clone(), auth.clone()));
        let first = ready(root.path(), recipe.session, None).await;
        let client = reqwest::Client::new();
        let url = format!("http://127.0.0.1:{}", first.port);
        assert_eq!(
            client.get(&url).send().await.unwrap().text().await.unwrap(),
            first.child_port.to_string()
        );
        let _ = client.get(format!("{url}/crash")).send().await;
        let second = ready(root.path(), recipe.session, Some(first.child_port)).await;
        assert_eq!(first.port, second.port);
        assert_eq!(first.url, second.url);
        assert_eq!(registrations.load(Ordering::SeqCst), 1);
        task.abort();
        let _ = task.await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        let restored = tokio::spawn(serve(root.path().into(), recipe.clone(), auth));
        let third = ready(root.path(), recipe.session, Some(second.child_port)).await;
        assert_eq!(third.port, first.port);
        assert_eq!(
            client.get(&url).send().await.unwrap().text().await.unwrap(),
            third.child_port.to_string()
        );
        forwarded.store(0, Ordering::SeqCst);
        tokio::time::timeout(Duration::from_secs(12), restored)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(!recipe_path(root.path(), recipe.session).exists());
        assert_eq!(
            registrations.load(Ordering::SeqCst),
            1,
            "revoked forwards must not be recreated"
        );
    }
    #[test]
    fn injected_port_does_not_shell_interpret_arguments() {
        let root = tempfile::tempdir().unwrap();
        let mut recipe = recipe(root.path(), String::new());
        recipe.args = vec![
            "--port".into(),
            "{port}".into(),
            "literal $(touch bad)".into(),
        ];
        let command = child_command(&recipe, 12345);
        let args: Vec<_> = command
            .as_std()
            .get_args()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args, ["--port", "12345", "literal $(touch bad)"]);
    }

    #[test]
    fn stopping_an_old_plugin_does_not_stop_its_replacement() {
        let root = tempfile::tempdir().unwrap();
        let recipe = recipe(root.path(), String::new());
        atomic_json(&recipe_path(root.path(), recipe.session), &recipe).unwrap();
        remove_at(root.path(), recipe.session, Some(Uuid::new_v4())).unwrap();
        assert!(current(root.path(), &recipe));
        remove_at(root.path(), recipe.session, Some(recipe.generation)).unwrap();
        assert!(!recipe_path(root.path(), recipe.session).exists());
    }
    #[tokio::test]
    async fn occupied_relay_port_is_not_adopted_or_reassigned() {
        let root = tempfile::tempdir().unwrap();
        let recipe = recipe(root.path(), String::new());
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        atomic_json(
            &status_path(root.path(), recipe.session),
            &Status {
                generation: recipe.generation,
                port,
                child_port: 0,
                url: Some("saved".into()),
                error: None,
                ready: false,
            },
        )
        .unwrap();
        let auth: Credentials = Arc::new(|| panic!("must not register a different service"));
        assert!(serve(root.path().into(), recipe, auth).await.is_err());
    }
}
