//! Stream screenshots directly to a private remote temporary directory. There
//! is no local screenshot file, and partial remote uploads are removed on error.
use super::*;

fn clipboard_upload_script(size: usize) -> String {
    format!(
        r#"set -eu
umask 077
directory=$(mktemp -d /tmp/ishell-clipboard-XXXXXXXXXX)
trap 'rm -f "$directory/screenshot.png"; rmdir "$directory" 2>/dev/null || :' EXIT
trap 'exit 1' HUP INT TERM
cat > "$directory/screenshot.png"
[ "$(wc -c < "$directory/screenshot.png")" -eq {size} ]
# Reclaim only this user's old iShell screenshots. A failed cleanup must not
# prevent pasting on hosts whose find does not support these options.
find /tmp -maxdepth 1 -type d -user "$(id -un)" -name 'ishell-clipboard-??????????' -mtime +7 -exec sh -c '
    for old do
        [ ! -L "$old" ] || continue
        rm -f "$old/screenshot.png"
        rmdir "$old" 2>/dev/null || :
    done
' sh {{}} + >/dev/null 2>&1 || :
printf '%s\n' "$directory/screenshot.png"
trap - EXIT HUP INT TERM
"#
    )
}

pub fn upload_clipboard_image(app: &AppHandle, id: &str, png: &[u8]) -> Result<String, String> {
    let output = run_remote_command(
        app,
        id,
        &format!(
            "sh -c {}",
            openssh::shell_quote(&clipboard_upload_script(png.len()))
        ),
        Some(png),
    )?;
    uploaded_image_path(&output)
}

fn uploaded_image_path(output: &str) -> Result<String, String> {
    let path = output.trim();
    // Only return a generated absolute path, never terminal control sequences or
    // startup-script output that could become commands when inserted in a shell.
    let token = path
        .strip_prefix("/tmp/ishell-clipboard-")
        .and_then(|path| path.strip_suffix("/screenshot.png"));
    if !token.is_some_and(|s| s.len() == 10 && s.bytes().all(|c| c.is_ascii_alphanumeric())) {
        return Err("截图已上传，但远程返回的图片路径无效".to_string());
    }
    Ok(path.to_string())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::{
        os::unix::fs::PermissionsExt,
        process::{Command, Stdio},
    };

    // Execute the actual remote script locally, with a private /tmp substitute
    // so the tests never inspect or clean the machine's real screenshot files.
    fn run_script(root: &Path, input: &[u8], expected: usize) -> std::process::Output {
        let script = clipboard_upload_script(expected)
            .replace("/tmp/", &format!("{}/", root.display()))
            .replace(
                "find /tmp ",
                &format!("find {} ", openssh::shell_quote(root.to_str().unwrap())),
            );
        let mut child = Command::new("sh")
            .args(["-c", &script])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(input).unwrap();
        child.wait_with_output().unwrap()
    }

    #[test]
    fn remote_path_validation_rejects_startup_output_and_shell_control_characters() {
        let valid = "/tmp/ishell-clipboard-aB01234567/screenshot.png";
        assert_eq!(uploaded_image_path(&format!("{valid}\n")).unwrap(), valid);
        for invalid in [
            format!("welcome\n{valid}"),
            format!("{valid}\nwhoami"),
            "/tmp/ishell-clipboard-0123456789/../screenshot.png".to_string(),
            "/tmp/ishell-clipboard-012345678;/screenshot.png".to_string(),
            "/tmp/ishell-clipboard-012345678\x1b/screenshot.png".to_string(),
        ] {
            assert!(uploaded_image_path(&invalid).is_err());
        }
    }

    #[test]
    fn cleanup_removes_only_old_screenshots_and_keeps_recent_and_unrelated_files() {
        use std::{fs::FileTimes, time::SystemTime};
        let root =
            std::env::temp_dir().join(format!("ishell-clipboard-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        let old_time = SystemTime::now() - Duration::from_secs(9 * 86400);
        let old = root.join("ishell-clipboard-0123456789");
        let recent = root.join("ishell-clipboard-abcdefghij");
        let mixed = root.join("ishell-clipboard-klmnopqrst");
        let unrelated = root.join("another-app-0123456789");
        for directory in [&old, &recent, &mixed, &unrelated] {
            fs::create_dir(directory).unwrap();
            fs::write(directory.join("screenshot.png"), b"keep or expire").unwrap();
        }
        fs::write(mixed.join("unrelated.txt"), b"must survive").unwrap();
        for directory in [&old, &mixed, &unrelated] {
            fs::File::open(directory)
                .unwrap()
                .set_times(FileTimes::new().set_modified(old_time))
                .unwrap();
        }
        let result = run_script(&root, b"png", 3);
        assert!(result.status.success());
        assert!(!old.exists());
        assert!(!mixed.join("screenshot.png").exists());
        assert_eq!(
            fs::read(mixed.join("unrelated.txt")).unwrap(),
            b"must survive"
        );
        assert!(recent.join("screenshot.png").exists());
        assert!(unrelated.join("screenshot.png").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn remote_screenshots_are_unique_private_complete_and_partial_uploads_are_removed() {
        let root =
            std::env::temp_dir().join(format!("ishell-clipboard-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        let bytes = b"\x89PNG\r\n\x1a\n\0\xffbinary";
        let first = run_script(&root, bytes, bytes.len());
        assert!(
            first.status.success(),
            "{}",
            String::from_utf8_lossy(&first.stderr)
        );
        let first_path = PathBuf::from(String::from_utf8(first.stdout).unwrap().trim());
        assert_eq!(fs::read(&first_path).unwrap(), bytes);
        assert_eq!(
            fs::metadata(&first_path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(first_path.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        let second = run_script(&root, bytes, bytes.len());
        assert!(second.status.success());
        assert_ne!(
            second.stdout,
            format!("{}\n", first_path.display()).as_bytes()
        );
        let failed = run_script(&root, bytes, bytes.len() + 1);
        assert!(!failed.status.success());
        assert!(failed.stdout.is_empty());
        assert_eq!(fs::read_dir(&root).unwrap().count(), 2);
        fs::remove_dir_all(root).unwrap();
    }
}
