//! One byte over the documented input limit is exit 2.
//! The file is sparse, lives in the system temp directory, and is removed
//! when the test ends. It is not a repository file.

use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

fn output_within(args: &[&str], limit: Duration) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_ci-bill"))
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("binary start");
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return child.wait_with_output().expect("output"),
            Ok(None) if start.elapsed() >= limit => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("timeout after {limit:?}, the process is killed, args: {args:?}");
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(err) => panic!("waiting for the process: {err}"),
        }
    }
}

#[test]
fn usage_over_the_byte_limit_exits_2() {
    const LIMIT: u64 = 268435456;
    struct Gone(std::path::PathBuf);
    impl Drop for Gone {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    let path = Gone(
        std::env::temp_dir().join(format!("ci-bill-w46-over-limit-{}.csv", std::process::id())),
    );
    let file =
        std::fs::File::create(&path.0).expect("create over-limit file in the temp directory");
    file.set_len(LIMIT + 1)
        .expect("sparse length one byte over the limit");
    drop(file);

    let shown = path.0.to_str().expect("path is utf-8");
    let output = output_within(
        &["--quiet", "report", "--usage", shown],
        Duration::from_secs(5),
    );
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
    let stderr = String::from_utf8(output.stderr).expect("stderr is utf-8");
    assert!(
        stderr.contains(&format!("file is larger than {LIMIT} bytes")),
        "{stderr}"
    );
    assert!(!stderr.contains("panicked"), "{stderr}");
}
