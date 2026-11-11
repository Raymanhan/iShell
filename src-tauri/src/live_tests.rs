//! End-to-end tests against real SSH servers, through the same functions the
//! commands call. They need servers set up for the purpose and a display (a
//! real Tauri app is built), so they only run when asked:
//!
//! ```text
//! ISHELL_LIVE_SSH=127.0.0.1:2222 ISHELL_LIVE_USER=tester \
//! ISHELL_LIVE_KEY=/path/to/private_key ISHELL_LIVE_PASSWORD=secret \
//! xvfb-run cargo test live_ -- --nocapture
//! ```
//!
//! Optional: `ISHELL_LIVE_SFTP_ONLY_USER` (an account restricted to
//! `internal-sftp`, same password) and `ISHELL_LIVE_NO_SFTP_PORT` (a server on
//! the same host without the SFTP subsystem). Add `--features force-russh`
//! to run the same checks on the other backend.

use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

use tauri::AppHandle;

use crate::{
    error::AppError,
    models::{PortForward, ServerRecord},
    pool::SshPool,
    ssh, store, transport,
};

struct Live {
    host: String,
    port: u16,
    user: String,
    key: String,
    password: String,
}

fn live() -> Option<Live> {
    let address = std::env::var("ISHELL_LIVE_SSH").ok()?;
    let (host, port) = address.rsplit_once(':')?;
    Some(Live {
        host: host.to_string(),
        port: port.parse().ok()?,
        user: std::env::var("ISHELL_LIVE_USER").ok()?,
        key: std::env::var("ISHELL_LIVE_KEY").ok()?,
        password: std::env::var("ISHELL_LIVE_PASSWORD").ok()?,
    })
}

fn server(id: &str, live: &Live, port: u16, user: &str, auth: &str) -> ServerRecord {
    ServerRecord {
        id: id.into(),
        name: id.into(),
        host: live.host.clone(),
        port,
        username: user.into(),
        group: "Live".into(),
        tags: vec![],
        auth_type: auth.into(),
        key_path: (auth == "key").then(|| live.key.clone()),
        color: String::new(),
        notes: String::new(),
        jump_server_id: None,
        encoding: "UTF-8".into(),
        sort_order: 0,
        created_at: 0,
        updated_at: 0,
        last_connected_at: None,
    }
}

fn shell(app: &AppHandle, id: &str, script: &str) -> String {
    let command = format!("sh -c {}", crate::openssh::shell_quote(script));
    String::from_utf8(transport::exec(app, id, &command, None).expect(script)).unwrap()
}

fn never() -> bool {
    false
}

/// File panel operations, the same on every kind of server.
fn file_panel(app: &AppHandle, id: &str, base: &str, has_shell: bool) {
    let pool = SshPool;
    let dir = format!("{base}/panel/深层 dir");
    ssh::make_directory(&pool, app, id, &dir).unwrap();
    ssh::make_directory(&pool, app, id, &dir).expect("existing directory is fine");
    assert_eq!(
        ssh::resolve_sftp_directory(app, id, &dir).unwrap(),
        dir,
        "absolute directories resolve to themselves"
    );
    assert!(ssh::resolve_sftp_directory(app, id, "~")
        .unwrap()
        .starts_with('/'));
    assert!(ssh::resolve_sftp_directory(app, id, &format!("{dir}/missing")).is_err());

    // Create a file by uploading, so this also works without a shell.
    let local = std::env::temp_dir().join(format!("ishell-live-{}.txt", uuid::Uuid::new_v4()));
    fs::write(&local, "第一版\n").unwrap();
    let uploaded = ssh::upload_file(
        &pool,
        app,
        id,
        local.to_str().unwrap(),
        &dir,
        "t-up",
        &never,
    )
    .unwrap();
    let file = format!("{dir}/{}", local.file_name().unwrap().to_str().unwrap());
    assert_eq!(uploaded, file);
    fs::remove_file(&local).ok();

    let entries = ssh::list_sftp(&pool, app, id, &dir).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].path, file);
    assert!(!entries[0].is_dir && !entries[0].is_symlink);
    assert_eq!(entries[0].size, Some("第一版\n".len() as u64));
    assert!(entries[0].owner.is_some(), "owner name is reported");
    assert!(entries[0].modified_at.unwrap() > 1_600_000_000);

    // Editing: read, save, and refuse to clobber someone else's change.
    let opened = ssh::read_text_file(&pool, app, id, &file).unwrap();
    assert_eq!(opened.content, "第一版\n");
    let saved = ssh::write_text_file(
        &pool,
        app,
        id,
        &file,
        "第二版\n更多\n",
        Some(&opened.revision),
    )
    .unwrap();
    assert_ne!(saved, opened.revision);
    assert_eq!(
        ssh::read_text_file(&pool, app, id, &file).unwrap().content,
        "第二版\n更多\n"
    );
    let stale = ssh::write_text_file(&pool, app, id, &file, "冲突", Some(&opened.revision));
    assert_eq!(stale.unwrap_err().code, AppError::FILE_CHANGED);
    assert_eq!(
        ssh::read_text_file(&pool, app, id, &file).unwrap().content,
        "第二版\n更多\n",
        "a refused save leaves the file alone"
    );
    ssh::write_text_file(&pool, app, id, &file, "强制覆盖", None).unwrap();
    assert_eq!(
        ssh::read_text_file(&pool, app, id, &file).unwrap().content,
        "强制覆盖"
    );
    let leftovers: Vec<_> = ssh::list_sftp(&pool, app, id, &dir)
        .unwrap()
        .into_iter()
        .filter(|entry| entry.name.contains(".ishell-"))
        .collect();
    assert!(
        leftovers.is_empty(),
        "no temporary files remain: {leftovers:?}"
    );

    let renamed = format!("{dir}/renamed.txt");
    ssh::rename_entry(&pool, app, id, &file, &renamed).unwrap();
    let names: Vec<_> = ssh::list_sftp(&pool, app, id, &dir)
        .unwrap()
        .into_iter()
        .map(|entry| entry.name)
        .collect();
    assert_eq!(names, ["renamed.txt"]);

    if has_shell {
        // Mode, owner and identity of a replaced file; symlinks.
        shell(app, id, &format!("chmod 640 '{renamed}' && ln -s renamed.txt '{dir}/link' && ln -s . '{dir}/dirlink'"));
        let inode_before = shell(app, id, &format!("stat -c %i '{renamed}'"));
        let via_link = format!("{dir}/link");
        let opened = ssh::read_text_file(&pool, app, id, &via_link).unwrap();
        ssh::write_text_file(
            &pool,
            app,
            id,
            &via_link,
            "经由链接保存\n",
            Some(&opened.revision),
        )
        .unwrap();
        let after = shell(
            app,
            id,
            &format!("stat -c '%a %U' '{renamed}'; test -L '{via_link}' && echo link-kept; cat '{renamed}'"),
        );
        let user = shell(app, id, "id -un");
        assert_eq!(after, format!("640 {}link-kept\n经由链接保存\n", user));
        if ssh::sftp_available_for_tests(app, id) {
            assert_ne!(
                shell(app, id, &format!("stat -c %i '{renamed}'")),
                inode_before,
                "an SFTP save replaces the file atomically"
            );
        }

        let entries = ssh::list_sftp(&pool, app, id, &dir).unwrap();
        let find = |name: &str| entries.iter().find(|entry| entry.name == name).unwrap();
        assert!(find("dirlink").is_dir && find("dirlink").is_symlink);
        assert_eq!(find("dirlink").target_is_dir, Some(true));
        assert_eq!(find("link").link_target.as_deref(), Some("renamed.txt"));
        assert!(!find("link").is_dir);
        assert_eq!(entries[0].name, "dirlink", "directories sort first");

        // Removing a link to a directory removes the link, not its target.
        ssh::remove_entry(&pool, app, id, &format!("{dir}/dirlink"), true).unwrap();
        assert!(ssh::list_sftp(&pool, app, id, &dir)
            .unwrap()
            .iter()
            .any(|entry| entry.name == "renamed.txt"));
    }

    ssh::remove_entry(&pool, app, id, &renamed, false).unwrap();
    ssh::remove_entry(&pool, app, id, &renamed, false).expect("already gone is fine");
    ssh::remove_entry(&pool, app, id, &format!("{base}/panel"), true).unwrap();
    assert!(ssh::list_sftp(&pool, app, id, base)
        .unwrap()
        .iter()
        .all(|entry| entry.name != "panel"));
}

fn sha(path: &Path) -> String {
    use sha2::{Digest, Sha256};
    let mut file = fs::File::open(path).unwrap();
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 1 << 16];
    loop {
        let read = file.read(&mut buffer).unwrap();
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    format!("{:x}", hasher.finalize())
}

/// Uploads and downloads of a file larger than one pipeline window, and of a
/// folder, compared byte for byte after the round trip.
fn transfers(app: &AppHandle, id: &str, base: &str, work: &Path) {
    let pool = SshPool;
    let remote_dir = format!("{base}/transfers");
    ssh::make_directory(&pool, app, id, &remote_dir).unwrap();

    // Not a multiple of any chunk size, and not compressible to nothing.
    let big = work.join(format!("big-{id}.bin"));
    let bytes: Vec<u8> = (0..5_000_123_u32)
        .map(|i| (i.wrapping_mul(2_654_435_761) >> 24) as u8)
        .collect();
    fs::write(&big, &bytes).unwrap();
    let remote_big = ssh::upload_file(
        &pool,
        app,
        id,
        big.to_str().unwrap(),
        &remote_dir,
        "t-big",
        &never,
    )
    .unwrap();
    let downloaded = ssh::download_file(&pool, app, id, &remote_big, "t-big-down", &never).unwrap();
    assert_eq!(sha(Path::new(&downloaded)), sha(&big), "file round trip");
    // Uploading again replaces the file rather than failing or appending.
    fs::write(&big, b"short").unwrap();
    ssh::upload_file(
        &pool,
        app,
        id,
        big.to_str().unwrap(),
        &remote_dir,
        "t-big2",
        &never,
    )
    .unwrap();
    let downloaded =
        ssh::download_file(&pool, app, id, &remote_big, "t-big-down2", &never).unwrap();
    assert_eq!(fs::read(&downloaded).unwrap(), b"short");

    let folder = work.join(format!("folder-{id}"));
    fs::create_dir_all(folder.join("sub/空 目录")).unwrap();
    fs::create_dir_all(folder.join("empty")).unwrap();
    fs::write(folder.join("a.txt"), "alpha").unwrap();
    fs::write(folder.join("sub/b.bin"), &bytes[..300_000]).unwrap();
    fs::write(folder.join("sub/空 目录/c.txt"), "γ").unwrap();
    let upload = |replace: bool, tid: &str| {
        ssh::upload_folder(
            &pool,
            app,
            id,
            folder.to_str().unwrap(),
            &remote_dir,
            tid,
            replace,
            &never,
        )
    };
    let remote_folder = upload(false, "t-folder").unwrap();
    assert!(
        upload(false, "t-folder-dup").is_err(),
        "existing folder is not merged into"
    );

    // A confirmed replace is a full replace: what was only remote goes away.
    fs::remove_file(folder.join("a.txt")).unwrap();
    fs::write(folder.join("new.txt"), "new").unwrap();
    upload(true, "t-folder-replace").unwrap();

    let local_copy = ssh::download_folder(
        &pool,
        app,
        id,
        &remote_folder,
        "t-folder-down",
        "raw",
        &never,
    )
    .unwrap();
    let local_copy = PathBuf::from(local_copy);
    assert!(!local_copy.join("a.txt").exists());
    assert_eq!(
        fs::read_to_string(local_copy.join("new.txt")).unwrap(),
        "new"
    );
    assert_eq!(
        sha(&local_copy.join("sub/b.bin")),
        sha(&folder.join("sub/b.bin"))
    );
    assert_eq!(
        fs::read_to_string(local_copy.join("sub/空 目录/c.txt")).unwrap(),
        "γ"
    );
    assert!(local_copy.join("empty").is_dir());

    // A stopped upload leaves no partial file behind.
    fs::write(&big, &bytes).unwrap();
    let stopped = ssh::upload_file(
        &pool,
        app,
        id,
        big.to_str().unwrap(),
        &format!(
            "{remote_dir}/{}",
            folder.file_name().unwrap().to_str().unwrap()
        ),
        "t-cancel",
        &|| true,
    );
    assert!(stopped.is_err());
    let names: Vec<_> = ssh::list_sftp(&pool, app, id, &remote_folder)
        .unwrap()
        .into_iter()
        .map(|entry| entry.name)
        .collect();
    assert!(
        names
            .iter()
            .all(|name| !name.starts_with(".ishell-") && !name.starts_with("big-")),
        "{names:?}"
    );

    ssh::remove_entry(&pool, app, id, &remote_dir, true).unwrap();
}

fn streaming(app: &AppHandle, id: &str, base: &str) {
    let log = format!("{base}/tail.log");
    shell(app, id, &format!("printf 'one\\ntwo\\n' > '{log}'"));
    let seen = Arc::new(Mutex::new(String::new()));
    let stop = Arc::new(AtomicBool::new(false));
    let (thread_app, thread_id, thread_log) = (app.clone(), id.to_string(), log.clone());
    let (thread_seen, thread_stop) = (seen.clone(), stop.clone());
    let tail = std::thread::spawn(move || {
        ssh::stream_tail_file(
            &thread_app,
            &thread_id,
            &thread_log,
            10,
            || thread_stop.load(Ordering::Relaxed),
            || {},
            |chunk| {
                thread_seen
                    .lock()
                    .unwrap()
                    .push_str(&String::from_utf8_lossy(chunk))
            },
        )
    });
    let wait_for = |needle: &str| {
        let deadline = Instant::now() + Duration::from_secs(15);
        while !seen.lock().unwrap().contains(needle) {
            assert!(Instant::now() < deadline, "tail never showed {needle:?}");
            std::thread::sleep(Duration::from_millis(50));
        }
    };
    wait_for("two");
    shell(app, id, &format!("echo three >> '{log}'"));
    wait_for("three");
    stop.store(true, Ordering::Relaxed);
    tail.join()
        .unwrap()
        .expect("stopping a stream is not an error");

    let status = ssh::fetch_status(&SshPool, app, id, true).unwrap();
    assert!(status.memory_total_mb.unwrap_or(0) > 0);
    assert!(!status.disk_mounts.is_empty());

    // The metrics stream: a round about every second, disk on the first.
    let samples = std::cell::RefCell::new(Vec::new());
    let started = Instant::now();
    ssh::stream_metrics(
        app,
        id,
        || samples.borrow().len() >= 3 || started.elapsed() > Duration::from_secs(20),
        |sample| samples.borrow_mut().push(sample),
    )
    .expect("stopping the metrics stream is not an error");
    let samples = samples.into_inner();
    assert!(samples.len() >= 3, "only {} rounds", samples.len());
    assert!(samples[0].has_disk && !samples[0].status.disk_mounts.is_empty());
    assert!(!samples[1].has_disk);
    assert!(samples
        .iter()
        .all(|s| s.status.memory_total_mb.unwrap_or(0) > 0));
    assert!(samples[2].network.sampled_at - samples[0].network.sampled_at >= 1.5);
}

fn port_forward(app: &AppHandle, id: &str, live: &Live) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let local_port = listener.local_addr().unwrap().port();
    drop(listener);
    let forward = PortForward {
        id: "live-forward".into(),
        server_id: id.into(),
        kind: "local".into(),
        bind_host: "127.0.0.1".into(),
        bind_port: local_port,
        dest_host: "127.0.0.1".into(),
        dest_port: live.port,
        description: String::new(),
        auto_start: false,
        created_at: 0,
        updated_at: 0,
    };
    let running = Arc::new(AtomicBool::new(false));
    let flag = running.clone();
    let stop = transport::start_forward(
        app,
        &forward,
        Box::new(move || flag.store(true, Ordering::Relaxed)),
        Box::new(|_| {}),
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    while !running.load(Ordering::Relaxed) {
        assert!(Instant::now() < deadline, "forward never started");
        std::thread::sleep(Duration::from_millis(50));
    }
    // The forwarded port reaches the SSH server itself, which greets first.
    let mut stream = std::net::TcpStream::connect(("127.0.0.1", local_port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut banner = [0_u8; 4];
    stream.read_exact(&mut banner).unwrap();
    assert_eq!(&banner, b"SSH-");
    stop();
}

/// An interactive session: login, typed input, non-UTF-8 output kept as is,
/// a late pane catching up, and closing.
fn terminal_session(app: &AppHandle, id: &str) {
    use crate::terminal::{self, FRAME_CLOSED, FRAME_DATA, FRAME_READY};
    use tauri::ipc::{Channel, InvokeResponseBody};

    type Frames = Arc<Mutex<Vec<(u8, u64, Vec<u8>)>>>;
    fn recorder() -> (Channel, Frames) {
        let frames: Frames = Arc::default();
        let sink = frames.clone();
        let channel = Channel::new(move |body| {
            if let InvokeResponseBody::Raw(bytes) = body {
                let offset = u64::from_le_bytes(bytes[1..9].try_into().unwrap());
                sink.lock()
                    .unwrap()
                    .push((bytes[0], offset, bytes[9..].to_vec()));
            }
            Ok(())
        });
        (channel, frames)
    }
    fn output(frames: &Frames) -> Vec<u8> {
        let mut bytes = Vec::new();
        for (kind, offset, payload) in frames.lock().unwrap().iter() {
            if *kind == FRAME_DATA {
                // Keep what is new past `offset`, as a pane does.
                let skip = (bytes.len() as u64).saturating_sub(*offset) as usize;
                bytes.extend_from_slice(&payload[skip.min(payload.len())..]);
            }
        }
        bytes
    }
    let wait = |what: &str, done: &dyn Fn() -> bool| {
        let deadline = Instant::now() + Duration::from_secs(20);
        while !done() {
            assert!(Instant::now() < deadline, "terminal never showed {what}");
            std::thread::sleep(Duration::from_millis(50));
        }
    };

    let registry = Arc::new(terminal::TerminalRegistry::default());
    let (first, first_frames) = recorder();
    let session = terminal::open_terminal(app.clone(), &registry, id.into(), Some(first)).unwrap();
    wait("ready", &|| {
        first_frames
            .lock()
            .unwrap()
            .iter()
            .any(|f| f.0 == FRAME_READY)
    });
    assert!(registry.connected_to(&session, id));

    // Bytes that are not UTF-8 ("中文" in GBK) must arrive untouched.
    terminal::terminal_input(
        &registry,
        &session,
        b"printf 'gbk:\\326\\320\\316\\304:end\\n'\n".to_vec(),
    )
    .unwrap();
    let gbk = b"gbk:\xd6\xd0\xce\xc4:end";
    wait("raw output", &|| {
        output(&first_frames).windows(gbk.len()).any(|w| w == gbk)
    });

    // A second pane gets the same output from the start.
    let (late, late_frames) = recorder();
    terminal::attach_terminal(&registry, &session, late).unwrap();
    assert_eq!(output(&late_frames), output(&first_frames));
    assert!(late_frames
        .lock()
        .unwrap()
        .iter()
        .any(|f| f.0 == FRAME_READY));

    // What the AI command shortcut sends: Ctrl+U clears the typed `# …`
    // line, then the suggestion is typed without being submitted.
    terminal::terminal_input(&registry, &session, b"# show listening ports".to_vec()).unwrap();
    terminal::terminal_input(&registry, &session, b"\x15echo replaced-$((40+2))".to_vec()).unwrap();
    std::thread::sleep(Duration::from_millis(500));
    let before_enter = String::from_utf8_lossy(&output(&first_frames)).to_string();
    assert!(
        !before_enter.contains("replaced-42"),
        "nothing ran before Enter"
    );
    terminal::terminal_input(&registry, &session, b"\r".to_vec()).unwrap();
    wait("the suggested command's output", &|| {
        String::from_utf8_lossy(&output(&first_frames)).contains("replaced-42")
    });
    assert_eq!(
        registry
            .recent_text(&session, 3)
            .unwrap()
            .matches("show listening ports")
            .count(),
        0,
        "the description line was cleared"
    );

    // Switched to GBK: typed text reaches the remote side as GBK bytes, and
    // GBK output reads back as text.
    assert_eq!(
        terminal::set_encoding(&registry, &session, "gbk").unwrap(),
        "GBK"
    );
    terminal::terminal_type(
        &registry,
        &session,
        "printf 中 | od -An -tx1; printf '\\304\\343\\272\\303-gbk\\n'\r",
    )
    .unwrap();
    wait("GBK input and output", &|| {
        registry
            .recent_text(&session, 6)
            .is_some_and(|text| text.contains("d6 d0") && text.contains("你好-gbk"))
    });
    terminal::set_encoding(&registry, &session, "utf-8").unwrap();

    terminal::terminal_resize(&registry, &session, 100, 30).unwrap();
    terminal::terminal_input(&registry, &session, b"stty size; exit\n".to_vec()).unwrap();
    wait("its own exit", &|| {
        late_frames
            .lock()
            .unwrap()
            .iter()
            .any(|f| f.0 == FRAME_CLOSED)
    });
    let text = String::from_utf8_lossy(&output(&late_frames)).to_string();
    assert!(
        text.contains("30 100"),
        "resize reached the remote PTY: {text}"
    );
    assert!(!registry.connected_to(&session, id));
    terminal::close_terminal(&registry, &session);
}

/// One AI tool call fanned out to several servers, through the same path an
/// AI task uses: each result must come from its own server, and a server
/// whose terminal is gone fails alone.
fn ai_fanout(app: &AppHandle, ids: &[&str]) {
    use crate::{ai::targets, terminal};
    use tauri::ipc::Channel;

    let registry = Arc::new(terminal::TerminalRegistry::default());
    let mut hosts = Vec::new();
    for id in ids {
        let session =
            terminal::open_terminal(app.clone(), &registry, id.to_string(), None).unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        while !registry.connected_to(&session, id) {
            assert!(Instant::now() < deadline, "{id} never connected");
            std::thread::sleep(Duration::from_millis(50));
        }
        hosts.push(targets::Host {
            label: id.to_string(),
            server: store::get_server(app, id).unwrap(),
            session,
        });
    }
    // The last host's terminal goes away before the call.
    let closed = hosts.last().unwrap().session.clone();
    terminal::close_terminal(&registry, &closed);

    let updates = Arc::new(Mutex::new(0));
    let counter = updates.clone();
    let events = Channel::new(move |_| {
        *counter.lock().unwrap() += 1;
        Ok(())
    });
    let chosen: Vec<&targets::Host> = hosts.iter().collect();
    let result = targets::run(
        app,
        &registry,
        &Arc::new(AtomicBool::new(false)),
        &events,
        "tool-1",
        "shell",
        &serde_json::json!({"command": "printf '%s' \"${SSH_CONNECTION##* }\""}),
        &chosen,
    )
    .unwrap();

    let results = result["results"].as_array().unwrap();
    assert_eq!(results.len(), ids.len());
    let port = |i: usize| results[i]["stdout"].as_str().unwrap().to_string();
    for (i, host) in hosts.iter().enumerate().take(ids.len() - 1) {
        assert_eq!(results[i]["server"], host.label);
        assert_eq!(results[i]["exitCode"], 0, "{}", results[i]);
        assert_eq!(
            port(i),
            host.server.port.to_string(),
            "ran on its own server"
        );
    }
    let last = &results[ids.len() - 1];
    assert!(
        last["error"].as_str().unwrap().contains("连接已关闭"),
        "{last}"
    );
    assert_eq!(result["exitCode"], 1);
    assert!(result["error"]
        .as_str()
        .unwrap()
        .contains(ids[ids.len() - 1]));
    assert!(
        *updates.lock().unwrap() > 0,
        "combined progress was reported"
    );
    for host in &hosts {
        terminal::close_terminal(&registry, &host.session);
    }
}

#[test]
fn live_ssh_end_to_end() {
    let Some(live) = live() else {
        eprintln!("ISHELL_LIVE_SSH not set: skipping the live SSH tests");
        return;
    };
    let work = std::env::temp_dir().join(format!("ishell-live-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(work.join("downloads")).unwrap();
    // Keep the test's database, key file and downloads away from real ones.
    std::env::set_var("XDG_DATA_HOME", work.join("data"));
    // The download folder comes from user-dirs.dirs, not the environment.
    fs::create_dir_all(work.join("config")).unwrap();
    fs::write(
        work.join("config/user-dirs.dirs"),
        format!(
            "XDG_DOWNLOAD_DIR=\"{}\"\n",
            work.join("downloads").display()
        ),
    )
    .unwrap();
    std::env::set_var("XDG_CONFIG_HOME", work.join("config"));
    std::env::set_var("ISHELL_SECRET_STORAGE", "file");

    let app = tauri::Builder::default()
        .any_thread()
        .build(tauri::generate_context!())
        .expect("a display is required: run under xvfb-run");
    let app = app.handle().clone();
    let data_dir = crate::secrets::app_data_dir(&app).unwrap();
    assert!(
        data_dir.starts_with(&work),
        "refusing to run against real app data at {data_dir:?}"
    );
    #[cfg(russh_backend)]
    {
        let handle = app.clone();
        crate::russh_transport::set_jump_resolver(Box::new(move |server| {
            store::jump_hops(&handle, server)
        }));
    }

    let by_key = server("live-key", &live, live.port, &live.user, "key");
    let by_password = server("live-password", &live, live.port, &live.user, "password");
    store::upsert_server(&app, &by_key, None).unwrap();
    store::upsert_server(&app, &by_password, Some(&live.password)).unwrap();

    transport::test_connection(&app, &by_key, None).expect("key login");
    transport::test_connection(&app, &by_password, Some(&live.password)).expect("password login");
    assert!(transport::test_connection(&app, &by_password, Some("wrong-password")).is_err());

    let home = shell(&app, "live-key", "printf %s \"$HOME\"");
    let base = format!("{home}/ishell-live-{}", uuid::Uuid::new_v4().simple());
    shell(&app, "live-key", &format!("mkdir -p '{base}'"));
    assert_eq!(
        transport::exec(&app, "live-password", "cat", Some("标准输入".as_bytes())).unwrap(),
        "标准输入".as_bytes()
    );
    let failure = transport::exec(&app, "live-key", "echo oops >&2; exit 3", None).unwrap_err();
    assert!(failure.contains("oops"), "{failure}");

    eprintln!("-- file panel over SFTP");
    assert!(ssh::sftp_available_for_tests(&app, "live-key"));
    file_panel(&app, "live-key", &base, true);
    eprintln!("-- command-based transfers");
    assert!(!ssh::sftp_transfers_for_tests(&app, "live-key", None));
    transfers(&app, "live-key", &base, &work);
    eprintln!("-- SFTP transfers");
    assert!(ssh::sftp_transfers_for_tests(
        &app,
        "live-password",
        Some(true)
    ));
    transfers(&app, "live-password", &base, &work);
    eprintln!("-- streaming and monitoring");
    streaming(&app, "live-key", &base);
    eprintln!("-- terminal");
    terminal_session(&app, "live-key");
    terminal_session(&app, "live-password");
    eprintln!("-- port forward");
    port_forward(&app, "live-key", &live);

    if let Ok(port) = std::env::var("ISHELL_LIVE_NO_SFTP_PORT") {
        eprintln!("-- server without the SFTP subsystem");
        let plain = server(
            "live-no-sftp",
            &live,
            port.parse().unwrap(),
            &live.user,
            "key",
        );
        store::upsert_server(&app, &plain, None).unwrap();
        assert!(!ssh::sftp_available_for_tests(&app, "live-no-sftp"));
        file_panel(&app, "live-no-sftp", &base, true);
    }

    if std::env::var("ISHELL_LIVE_NO_SFTP_PORT").is_ok() {
        eprintln!("-- AI tool call on several servers");
        ai_fanout(&app, &["live-key", "live-no-sftp", "live-password"]);
    }

    if let Ok(user) = std::env::var("ISHELL_LIVE_SFTP_ONLY_USER") {
        eprintln!("-- account restricted to SFTP");
        let restricted = server("live-sftp-only", &live, live.port, &user, "password");
        store::upsert_server(&app, &restricted, Some(&live.password)).unwrap();
        let home = ssh::resolve_sftp_directory(&app, "live-sftp-only", "~").unwrap();
        let base = format!("{home}/ishell-live-{}", uuid::Uuid::new_v4().simple());
        ssh::make_directory(&SshPool, &app, "live-sftp-only", &base).unwrap();
        assert!(ssh::sftp_transfers_for_tests(&app, "live-sftp-only", None));
        file_panel(&app, "live-sftp-only", &base, false);
        transfers(&app, "live-sftp-only", &base, &work);
        // With no shell to run `rm -rf`, a tree is removed over SFTP.
        ssh::make_directory(&SshPool, &app, "live-sftp-only", &format!("{base}/x/y/z")).unwrap();
        ssh::remove_entry(&SshPool, &app, "live-sftp-only", &base, true).unwrap();
        assert!(ssh::list_sftp(&SshPool, &app, "live-sftp-only", &home)
            .unwrap()
            .iter()
            .all(|entry| !entry.name.starts_with("ishell-live-")));
    }

    shell(&app, "live-key", &format!("rm -rf '{base}'"));
    for id in [
        "live-key",
        "live-password",
        "live-no-sftp",
        "live-sftp-only",
    ] {
        transport::invalidate(id);
    }
    fs::remove_dir_all(&work).ok();
}
