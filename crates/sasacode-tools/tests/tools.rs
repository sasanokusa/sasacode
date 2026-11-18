//! packages/tools/test/tools.test.ts, ported case by case. Each test has its own sandbox
//! directory (cargo runs tests in parallel; the TS ones shared one directory in sequence).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use sasacode_plugin_api::{JsonObject, Tool, ToolContext, ToolResult, UserContent};
use sasacode_tools::{BashTool, EditTool, ReadTool, WriteTool, builtin_tools};

fn args(v: Value) -> JsonObject {
    v.as_object().cloned().expect("arguments are an object")
}

fn ctx(dir: &Path) -> ToolContext {
    ToolContext::new(dir)
}

/// `out(r)`: the text parts, joined.
fn out(r: &ToolResult) -> String {
    r.content.iter().map(|c| c.as_text().unwrap_or("")).collect()
}

async fn run(tool: &dyn Tool, dir: &Path, a: Value) -> ToolResult {
    tool.execute(args(a), ctx(dir))
        .await
        .expect("the tool returns a result")
}

fn sandbox() -> tempfile::TempDir {
    tempfile::Builder::new().prefix("sasacode-tools-").tempdir().unwrap()
}

fn s(p: &Path) -> String {
    p.display().to_string()
}

fn read(p: impl AsRef<Path>) -> String {
    std::fs::read_to_string(p).unwrap()
}

/// The TS suite set SASACODE_HOME (where long outputs are saved) for the whole run. Setting it in
/// this process would need `unsafe` (edition 2024), so a test that saves output runs again in a
/// child test process with SASACODE_HOME in a sandbox. True in that child: run the body there.
fn in_sandboxed_home(test: &str) -> bool {
    if std::env::var_os("SASACODE_TOOLS_TEST_HOME").is_some() {
        return true;
    }
    let home = sandbox();
    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([test, "--exact", "--nocapture", "--test-threads=1"])
        .env("SASACODE_TOOLS_TEST_HOME", "1")
        .env("SASACODE_HOME", home.path().join(".home"))
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&child.stdout);
    assert!(
        child.status.success() && stdout.contains("1 passed"),
        "{test} in its sandbox:\n{stdout}{}",
        String::from_utf8_lossy(&child.stderr)
    );
    false
}

/// The path in "... saved to <path>]".
fn saved_file(text: &str) -> Option<PathBuf> {
    let at = text.find("saved to ")? + "saved to ".len();
    let end = at + text[at..].find(']')?;
    Some(PathBuf::from(&text[at..end]))
}

#[tokio::test]
async fn write_creates_parents_read_numbers_lines_and_pages() {
    let d = sandbox();
    let dir = d.path();
    run(
        &WriteTool,
        dir,
        json!({ "path": "a/b/c.txt", "content": "one\ntwo\nthree\n" }),
    )
    .await;
    assert_eq!(read(dir.join("a/b/c.txt")), "one\ntwo\nthree\n");
    let all = out(&run(&ReadTool, dir, json!({ "path": "a/b/c.txt" })).await);
    assert_eq!(all, "     1\tone\n     2\ttwo\n     3\tthree");
    let page = out(&run(&ReadTool, dir, json!({ "path": "a/b/c.txt", "offset": 2, "limit": 1 })).await);
    assert!(page.contains("     2\ttwo"));
    assert!(page.contains("Showing lines 2-2 of 3. Use offset=3 to continue."));
}

#[tokio::test]
async fn read_returns_images_as_images_and_reports_missing_files() {
    let d = sandbox();
    let dir = d.path();
    std::fs::write(dir.join("x.png"), [0x89, 0x50, 0x4e, 0x47]).unwrap();
    let r = run(&ReadTool, dir, json!({ "path": "x.png" })).await;
    match &r.content[1] {
        UserContent::Image(img) => {
            assert_eq!(img.media_type, "image/png");
            assert_eq!(&*img.data, "iVBORw==");
        }
        other => panic!("expected an image, got {other:?}"),
    }
    assert_eq!(out(&r), format!("Image {} (4 bytes)", s(&dir.join("x.png"))));
    let missing = run(&ReadTool, dir, json!({ "path": "nope.txt" })).await;
    assert_eq!(missing.is_error, Some(true));
}

#[tokio::test]
async fn edit_requires_a_unique_exact_match_and_returns_a_diff() {
    let d = sandbox();
    let dir = d.path();
    std::fs::write(dir.join("e.txt"), "foo\nbar\nfoo\n").unwrap();
    let missing = run(
        &EditTool,
        dir,
        json!({ "path": "e.txt", "old_string": "baz", "new_string": "x" }),
    )
    .await;
    assert_eq!(missing.is_error, Some(true));
    let dup = run(
        &EditTool,
        dir,
        json!({ "path": "e.txt", "old_string": "foo", "new_string": "x" }),
    )
    .await;
    assert_eq!(dup.is_error, Some(true));
    assert!(out(&dup).contains("appears 2 times"));
    let ok = run(
        &EditTool,
        dir,
        json!({ "path": "e.txt", "old_string": "bar", "new_string": "BAR" }),
    )
    .await;
    assert!(!ok.is_error());
    let diff = ok.details.as_ref().unwrap()["diff"].as_str().unwrap().to_string();
    assert!(diff.contains("-bar\n+BAR"));
    run(
        &EditTool,
        dir,
        json!({ "path": "e.txt", "old_string": "foo", "new_string": "$&", "replace_all": true }),
    )
    .await;
    assert_eq!(read(dir.join("e.txt")), "$&\nBAR\n$&\n");
}

/// The near-miss hints: where an old_string that is not in the file comes closest, numbered as read
/// numbers lines, and where one found more than once is.
#[tokio::test]
async fn edit_errors_show_the_nearest_lines_and_where_the_matches_are() {
    let d = sandbox();
    let dir = d.path();
    let edit = |old: &str| json!({ "path": "c.ts", "old_string": old, "new_string": "X" });
    let file = "import x from \"y\";\n\nexport function count(s: string, p: string) {\n  const n = s.split(p).length - 2;\n  return n;\n}\n";
    std::fs::write(dir.join("c.ts"), file).unwrap();
    let missing = run(&EditTool, dir, edit("const n = s.split(p).length - 1;\nreturn n;")).await;
    assert_eq!(missing.is_error, Some(true));
    let text = out(&missing);
    let hint = "The lines most like it:\n     3\texport function count(s: string, p: string) {\n     4\t  const n = s.split(p).length - 2;\n     5\t  return n;\n     6\t}";
    assert!(text.ends_with(hint), "{text}");
    // The hint as given (read's form) is an old_string edit takes back.
    let numbered = "     4\t  const n = s.split(p).length - 2;\n     5\t  return n;";
    let fixed = json!({ "path": "c.ts", "old_string": numbered, "new_string": "  const n = s.split(p).length - 1;\n  return n;" });
    assert!(!run(&EditTool, dir, fixed).await.is_error());
    assert!(read(dir.join("c.ts")).contains("length - 1;"));

    // Nothing alike: no lines.
    let unlike = out(&run(&EditTool, dir, edit("zzzz qqqq")).await);
    assert!(unlike.ends_with("Re-read the file and match it exactly."), "{unlike}");
    // At most ten of old_string's lines, and one line each side.
    let long: String = (0..40).map(|i| format!("line {i}\n")).collect();
    std::fs::write(dir.join("c.ts"), &long).unwrap();
    let old: String = (5..30).map(|i| format!("line {i}\n")).collect::<String>() + "nope";
    let bounded = out(&run(&EditTool, dir, edit(&old)).await);
    let shown: Vec<&str> = bounded.split('\n').skip(1).collect();
    assert_eq!(shown.len(), 12, "{bounded}");
    assert_eq!((shown[0], shown[11]), ("     5\tline 4", "    16\tline 15"));

    // Found more than once: the lines, the first ten of them.
    std::fs::write(dir.join("c.ts"), "a\nb\na\n").unwrap();
    let twice = out(&run(&EditTool, dir, edit("a")).await);
    assert!(
        twice.contains("appears 2 times in") && twice.contains(" (lines 1, 3). "),
        "{twice}"
    );
    std::fs::write(dir.join("c.ts"), "t\n".repeat(12)).unwrap();
    let many = out(&run(&EditTool, dir, edit("t")).await);
    assert!(
        many.contains("appears 12 times in") && many.contains(" (lines 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, …). "),
        "{many}"
    );
    // Also when found only loosely (trailing whitespace ignored).
    std::fs::write(dir.join("c.ts"), "a  \nb\nx\na \nb\n").unwrap();
    let loose = out(&run(&EditTool, dir, edit("a\nb")).await);
    assert!(
        loose.contains("more than once") && loose.contains(" (lines 1, 4). "),
        "{loose}"
    );
}

#[tokio::test]
async fn bash_reports_exit_codes_truncates_to_a_file_and_times_out() {
    if !in_sandboxed_home("bash_reports_exit_codes_truncates_to_a_file_and_times_out") {
        return;
    }
    let d = sandbox();
    let dir = d.path();
    let r = run(&BashTool, dir, json!({ "command": "echo hi; exit 3" })).await;
    assert_eq!(out(&r), "hi\n[exit code 3]");
    assert_eq!(r.is_error, Some(true));
    let big = run(&BashTool, dir, json!({ "command": "yes x | head -c 40000" })).await;
    let file = saved_file(&out(&big)).expect("a saved file");
    assert_eq!(read(&file).len(), 40000);
    let slow = run(&BashTool, dir, json!({ "command": "sleep 5", "timeout": 0.3 })).await;
    assert!(out(&slow).contains("timed out after 0.3s"));
}

/// The test owns its runtime: dropping it waits for the threads still reading the command's output
/// (Windows reads child pipes on blocking threads), so a process the abort left running shows up as
/// time. On Windows, before job objects, that was 30 s: the whole `sleep 30`.
#[test]
fn bash_abort_kills_the_whole_process_group() {
    let d = sandbox();
    let c = ctx(d.path());
    let signal = c.signal.clone();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let t0 = Instant::now();
    let r = rt.block_on(async move {
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            signal.cancel();
        });
        let command = json!({ "command": "sleep 30 & sleep 30; echo done" });
        BashTool.execute(args(command), c).await.unwrap()
    });
    assert!(t0.elapsed() < Duration::from_millis(3000));
    drop(rt);
    let held = t0.elapsed();
    assert!(
        held < Duration::from_secs(10),
        "the command's output was held for {held:?}"
    );
    assert!(out(&r).contains("interrupted by the user"));
    assert_eq!(r.details.as_ref().unwrap()["killedBy"], "abort");
}

#[tokio::test]
async fn bash_a_character_split_across_two_writes_stays_whole() {
    let d = sandbox();
    // "あ" is e3 81 82: the first two bytes, a pause, then the last one.
    let r = run(
        &BashTool,
        d.path(),
        json!({ "command": "printf '\\343\\201'; sleep 0.2; printf '\\202\\n'" }),
    )
    .await;
    assert!(out(&r).starts_with("あ\n"));
}

#[tokio::test]
async fn bash_a_flood_of_output_is_kept_to_its_tail_in_memory_and_saved_whole_privately() {
    if !in_sandboxed_home("bash_a_flood_of_output_is_kept_to_its_tail_in_memory_and_saved_whole_privately") {
        return;
    }
    let d = sandbox();
    let r = run(
        &BashTool,
        d.path(),
        json!({ "command": "for i in $(seq 1 20000); do echo line-$i; done" }),
    )
    .await;
    let text = out(&r);
    let file = saved_file(&text).unwrap();
    let home = PathBuf::from(std::env::var_os("SASACODE_HOME").unwrap());
    assert!(file.starts_with(home.join("tmp")));
    assert_eq!(read(&file).split('\n').next(), Some("line-1"));
    assert!(text.contains("line-20000"));
    // Unix permission bits; on Windows the file is private through the ACL of the user profile it is in.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600);
    }
}

#[tokio::test]
async fn read_pipes_and_devices_are_refused_big_images_too_big_files_read_only_as_far_as_shown() {
    let d = sandbox();
    let dir = d.path();
    if cfg!(unix) {
        // Windows has no named pipes in the file system.
        let made = std::process::Command::new("mkfifo")
            .arg(dir.join("fifo"))
            .status()
            .unwrap();
        assert!(made.success());
        assert!(out(&run(&ReadTool, dir, json!({ "path": "fifo" })).await).contains("not a regular file"));
    }
    std::fs::write(dir.join("huge.png"), vec![0u8; 5_000_001]).unwrap();
    assert!(out(&run(&ReadTool, dir, json!({ "path": "huge.png" })).await).contains("over the 5000000 bytes"));
    std::fs::write(dir.join("big.log"), format!("{}\n", "x".repeat(99)).repeat(120_000)).unwrap(); // 12 MB
    let r = out(&run(&ReadTool, dir, json!({ "path": "big.log", "limit": 3 })).await);
    assert!(r.contains("Showing lines 1-3 of a 12 MB file. Use offset=4"));
    // 100,000 characters come before 2,000 lines here, and the note says so in its own words (#66).
    let r = out(&run(&ReadTool, dir, json!({ "path": "big.log" })).await);
    assert!(r.contains("Showing lines 1-934 of a 12 MB file. Use offset=935"));
}

#[tokio::test]
async fn an_identical_write_is_a_no_op_contents_mtime_and_inode_stay_unchanged() {
    let d = sandbox();
    let dir = d.path();
    let path = dir.join("same.txt");
    let p = s(&path);
    let first = run(&WriteTool, dir, json!({ "path": p, "content": "same" })).await;
    assert_eq!(first.details.as_ref().unwrap()["changed"], true);
    let old = std::time::UNIX_EPOCH + Duration::from_secs(1);
    std::fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(old)
        .unwrap();
    let before = std::fs::metadata(&path).unwrap();
    let result = run(&WriteTool, dir, json!({ "path": p, "content": "same" })).await;
    let details = result.details.as_ref().unwrap();
    assert_eq!(details["changed"], false);
    assert!(details.get("diff").is_none());
    let after = std::fs::metadata(&path).unwrap();
    assert_eq!(after.modified().unwrap(), before.modified().unwrap());
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        assert_eq!(after.ino(), before.ino());
    }
    assert_eq!(read(&path), "same");
    let changed = run(&WriteTool, dir, json!({ "path": p, "content": "changed" })).await;
    assert_eq!(changed.details.as_ref().unwrap()["changed"], true);
}

#[tokio::test]
async fn write_counts_lines_as_read_does_a_final_newline_is_not_one_more_empty_is_none() {
    let d = sandbox();
    for (content, n) in [("hello\n", 1), ("a\nb\n", 2), ("", 0), ("no-newline", 1)] {
        let path = format!("count-{n}-{}.txt", content.len());
        let r = run(&WriteTool, d.path(), json!({ "path": path, "content": content })).await;
        assert!(
            serde_json::to_string(&r.content)
                .unwrap()
                .contains(&format!("({n} lines)"))
        );
    }
}

#[tokio::test]
async fn edit_an_exact_match_is_replaced_as_given_with_no_note() {
    let d = sandbox();
    let dir = d.path();
    std::fs::write(dir.join("plain.txt"), "\tkeep\r\nme\r\n").unwrap();
    let r = run(
        &EditTool,
        dir,
        json!({ "path": "plain.txt", "old_string": "me", "new_string": "you\nthere" }),
    )
    .await;
    assert_eq!(
        out(&r),
        format!("Edited {} (1 replacement).", s(&dir.join("plain.txt")))
    );
    assert_eq!(read(dir.join("plain.txt")), "\tkeep\r\nyou\nthere\r\n");
}

#[tokio::test]
async fn edit_reads_line_numbers_are_ignored_when_every_line_of_old_string_has_one() {
    let d = sandbox();
    let dir = d.path();
    let path = s(&dir.join("numbered.ts"));
    std::fs::write(&path, "function f() {\n\n  return 1;\n}\n").unwrap();
    let r = run(
        &EditTool,
        dir,
        json!({ "path": path, "old_string": "     1\tfunction f() {\n     2\t\n     3\t  return 1;",
            "new_string": "     1\tfunction f() {\n     3\t  return 2;" }),
    )
    .await;
    assert_eq!(
        out(&r),
        format!("[harness matched old_string ignoring line-number prefixes]\nEdited {path} (1 replacement).")
    );
    assert_eq!(read(&path), "function f() {\n  return 2;\n}\n");
    // One line without a number: old_string is taken as it is, and is not found.
    let mixed = run(
        &EditTool,
        dir,
        json!({ "path": path, "old_string": "     1\tfunction f() {\n  return 2;", "new_string": "x" }),
    )
    .await;
    assert!(out(&mixed).contains("not found"));
    assert_eq!(read(&path), "function f() {\n  return 2;\n}\n");
}

#[tokio::test]
async fn edit_a_tsv_rows_123_tab_is_the_files_text_not_reads_6_column_line_number() {
    let d = sandbox();
    let dir = d.path();
    let path = s(&dir.join("rows.tsv"));
    std::fs::write(&path, "123\tfoo\n456\tbar\nfoo\nbar\n").unwrap();
    // A space too many at the end: matched ignoring it, on the TSV rows, not on the plain lines below.
    let r = run(
        &EditTool,
        dir,
        json!({ "path": path, "old_string": "123\tfoo \n456\tbar", "new_string": "123\tFOO\n456\tBAR" }),
    )
    .await;
    assert!(out(&r).starts_with("[harness matched old_string ignoring trailing whitespace and line endings]\n"));
    assert_eq!(read(&path), "123\tFOO\n456\tBAR\nfoo\nbar\n");
    // read's own numbers on such a row are still taken off.
    let r = run(
        &EditTool,
        dir,
        json!({ "path": path, "old_string": "     1\t123\tFOO", "new_string": "     1\t123\tF" }),
    )
    .await;
    assert!(out(&r).starts_with("[harness matched old_string ignoring line-number prefixes]\n"));
    assert_eq!(read(&path), "123\tF\n456\tBAR\nfoo\nbar\n");
}

#[tokio::test]
async fn edit_a_number_past_the_files_last_line_is_no_line_number_of_reads() {
    let d = sandbox();
    let dir = d.path();
    let path = s(&dir.join("short.txt"));
    std::fs::write(&path, "hello\nfoo\nbar\n").unwrap();
    let r = run(
        &EditTool,
        dir,
        json!({ "path": path, "old_string": "123456\tfoo", "new_string": "BAZ" }),
    )
    .await;
    assert!(out(&r).contains("not found"));
    assert_eq!(read(&path), "hello\nfoo\nbar\n");
    std::fs::write(&path, "123457\tfoo\n234567\tbar\n").unwrap();
    let r = run(
        &EditTool,
        dir,
        json!({ "path": path, "old_string": "123456\tfoo", "new_string": "123456\tBAZ" }),
    )
    .await;
    assert!(out(&r).contains("not found"));
    assert_eq!(read(&path), "123457\tfoo\n234567\tbar\n");
    // Within the file, read's numbers are still taken off.
    let r = run(
        &EditTool,
        dir,
        json!({ "path": path, "old_string": "     2\t234567\tbar", "new_string": "     2\t234567\tBAR" }),
    )
    .await;
    assert!(out(&r).starts_with("[harness matched old_string ignoring line-number prefixes]\n"));
    assert_eq!(read(&path), "123457\tfoo\n234567\tBAR\n");
}

#[tokio::test]
async fn edit_trailing_whitespace_and_crlf_are_ignored_both_ways_and_the_file_keeps_its_line_endings() {
    let d = sandbox();
    let dir = d.path();
    let crlf = s(&dir.join("crlf.txt"));
    std::fs::write(&crlf, "a\r\nb  \r\nc\r\n").unwrap();
    let r = run(
        &EditTool,
        dir,
        json!({ "path": crlf, "old_string": "a\nb\nc", "new_string": "a\nB\nc" }),
    )
    .await;
    assert!(out(&r).starts_with("[harness matched old_string ignoring trailing whitespace and line endings]\n"));
    assert_eq!(read(&crlf), "a\r\nB\r\nc\r\n");
    let lf = s(&dir.join("lf.txt"));
    std::fs::write(&lf, "x\ny\nz\n").unwrap();
    let r = run(
        &EditTool,
        dir,
        json!({ "path": lf, "old_string": "x\r\ny \r\n", "new_string": "x\r\nY\r\n" }),
    )
    .await;
    assert!(!r.is_error());
    assert_eq!(read(&lf), "x\nY\nz\n");
    // Found once only the whitespace at the end is dropped: the note still names what was ignored.
    let end = run(
        &EditTool,
        dir,
        json!({ "path": lf, "old_string": "Y\nz  ", "new_string": "Y\nZ" }),
    )
    .await;
    assert!(out(&end).starts_with("[harness matched old_string ignoring trailing whitespace and line endings]\n"));
    assert_eq!(read(&lf), "x\nY\nZ\n");
    // The same past line numbers: both are named.
    let numbered = run(
        &EditTool,
        dir,
        json!({ "path": lf, "old_string": "     1\tx  ", "new_string": "     1\tX" }),
    )
    .await;
    assert!(out(&numbered).starts_with(
        "[harness matched old_string ignoring line-number prefixes, trailing whitespace and line endings]\n"
    ));
    assert_eq!(read(&lf), "X\nY\nZ\n");
}

#[tokio::test]
async fn edit_other_indentation_is_ignored_and_new_string_takes_the_files() {
    let d = sandbox();
    let dir = d.path();
    let path = s(&dir.join("indent.ts"));
    std::fs::write(&path, "function f() {\n\tif (x) {\n\t\treturn 1;\n\t}\n}\n").unwrap();
    // Dedented by a level: every line of new_string gets the file's indentation in front.
    let r = run(
        &EditTool,
        dir,
        json!({ "path": path, "old_string": "if (x) {\n\treturn 1;\n}", "new_string": "if (x) {\n\treturn 2;\n}" }),
    )
    .await;
    assert!(
        out(&r)
            .starts_with("[harness matched old_string ignoring trailing whitespace and line endings, indentation]\n")
    );
    assert_eq!(read(&path), "function f() {\n\tif (x) {\n\t\treturn 2;\n\t}\n}\n");
    // Spaces for tabs: each indentation old_string has becomes the file's for it, at every depth.
    let spaces = json!({ "path": path, "old_string": "    if (x) {\n        return 2;\n    }\n",
        "new_string": "    if (y) {\n        return 3;\n    }\n" });
    assert!(!run(&EditTool, dir, spaces).await.is_error());
    assert_eq!(read(&path), "function f() {\n\tif (y) {\n\t\treturn 3;\n\t}\n}\n");
}

#[tokio::test]
async fn edit_lines_shallower_than_the_first_and_other_indentation_units_land_at_the_files_depths() {
    let d = sandbox();
    let dir = d.path();
    let cases = [
        // A whole snippet dedented by one level that runs past the end of a block.
        (
            "def f(a):\n    if a:\n        x = 1\n    y = 2\n    return x + y\n",
            "    x = 1\ny = 2",
            "    x = 3\ny = 2",
            "        x = 3\n    y = 2\n    return x + y\n",
        ),
        (
            "function f() {\n  if (a) {\n    return 1;\n  }\n}\n",
            "  return 1;\n}",
            "  return 2;\n}",
            "    return 2;\n  }\n}\n",
        ),
        // A depth old_string does not have takes the one shift its lines share.
        (
            "function f() {\n  if (a) {\n    return 1;\n  }\n}\n",
            "  return 1;\n}",
            "  if (b)\n    return 2;\n}",
            "    if (b)\n      return 2;\n  }\n}\n",
        ),
        // Tabs in a file of spaces, four spaces in a file of tabs, two spaces in a file of four.
        (
            "def f(x):\n    if x:\n        a()\n        b()\n",
            "\tif x:\n\t\ta()",
            "\tif x:\n\t\ta2()",
            "    if x:\n        a2()\n        b()\n",
        ),
        (
            "def f(x):\n\tif x:\n\t\ta()\n\t\tb()\n",
            "    if x:\n        a()",
            "    if x:\n        a2()",
            "\tif x:\n\t\ta2()\n\t\tb()\n",
        ),
        (
            "def f(x):\n    if x:\n        a()\n        b()\n",
            "  if x:\n    a()",
            "  if x:\n    a2()",
            "    if x:\n        a2()\n        b()\n",
        ),
        (
            "class A:\n    def f(self):\n        return x\n\n    def g(self):\n",
            "    return x\n\n  def g(self):",
            "    return y\n\n  def g(self):",
            "        return y\n\n    def g(self):\n",
        ),
    ];
    let path = s(&dir.join("depths.txt"));
    for (file, old_string, new_string, tail) in cases {
        std::fs::write(&path, file).unwrap();
        let r = run(
            &EditTool,
            dir,
            json!({ "path": path, "old_string": old_string, "new_string": new_string }),
        )
        .await;
        assert!(
            out(&r).contains("ignoring trailing whitespace and line endings, indentation]"),
            "{}",
            out(&r)
        );
        assert!(read(&path).ends_with(tail), "{file:?} became {:?}", read(&path));
    }
    // A depth old_string does not have, with no one shift for all its lines: refused rather than guessed.
    let file = "f() {\n\tif (x) {\n\t\treturn 1;\n\t}\n}\n";
    std::fs::write(&path, file).unwrap();
    let deeper = json!({ "path": path, "old_string": "    if (x) {\n        return 1;\n    }",
        "new_string": "    if (x) {\n        if (y)\n            log();\n        return 1;\n    }" });
    let r = run(&EditTool, dir, deeper).await;
    assert_eq!(r.is_error, Some(true));
    assert!(out(&r).contains("indentation does not map onto the file's"));
    assert_eq!(read(&path), file);
}

#[tokio::test]
async fn edit_a_depth_old_string_lacks_takes_the_shared_shift_only_when_two_depths_or_unmoved_lines_show_it() {
    let d = sandbox();
    let dir = d.path();
    let path = s(&dir.join("onedepth.py"));
    // One depth shows no unit: "    \tz()", "      z()" and "\t    z()" would be TabError or IndentationError.
    for (file, old_string, new_string) in [
        (
            "def f(x):\n    if x:\n        a()\n    return 1\n",
            "\tif x:",
            "\tif x:\n\t\tz()",
        ),
        (
            "def f(x):\n  if x:\n    a()\n  return 1\n",
            "    if x:",
            "    if x:\n        z()",
        ),
        (
            "def f(x):\n\tif x:\n\t\ta()\n\treturn 1\n",
            "    if x:",
            "    if x:\n        z()",
        ),
    ] {
        std::fs::write(&path, file).unwrap();
        let r = run(
            &EditTool,
            dir,
            json!({ "path": path, "old_string": old_string, "new_string": new_string }),
        )
        .await;
        assert_eq!(r.is_error, Some(true));
        assert!(out(&r).contains("indentation does not map onto the file's"));
        assert_eq!(read(&path), file);
    }
    // Matched past line numbers or trailing whitespace, the lines did not move, so a new depth is kept as written.
    std::fs::write(&path, "foo()\n").unwrap();
    run(
        &EditTool,
        dir,
        json!({ "path": path, "old_string": "     1\tfoo()", "new_string": "     1\tfoo()\n     2\t  bar()" }),
    )
    .await;
    assert_eq!(read(&path), "foo()\n  bar()\n");
}

#[tokio::test]
async fn edit_a_loose_match_must_be_unique_replace_all_or_not_nothing_is_changed_otherwise() {
    let d = sandbox();
    let dir = d.path();
    let path = s(&dir.join("twice.ts"));
    std::fs::write(&path, "\tfoo();\n  foo();\n").unwrap();
    for replace_all in [false, true] {
        let r = run(
            &EditTool,
            dir,
            json!({ "path": path, "old_string": "    foo();", "new_string": "bar();", "replace_all": replace_all }),
        )
        .await;
        assert_eq!(r.is_error, Some(true));
        assert!(out(&r).contains("appears more than once"));
    }
    let none = run(
        &EditTool,
        dir,
        json!({ "path": path, "old_string": "    baz();", "new_string": "bar();" }),
    )
    .await;
    assert!(out(&none).contains("even ignoring"));
    // Blank once trailing whitespace is ignored: that is everywhere, so it is refused as ambiguous.
    let blank = run(
        &EditTool,
        dir,
        json!({ "path": path, "old_string": " \t ", "new_string": "x" }),
    )
    .await;
    assert!(out(&blank).contains("appears more than once"));
    assert_eq!(read(&path), "\tfoo();\n  foo();\n");
    // Overlapping places count too: with indentation ignored, nested closers fit at lines 3-4 and at 4-5.
    for (file, old_string) in [
        ("func f() {\n\tif x {\n\t\t}\n\t}\n}\n", "    }\n}"),
        ("a \na \na \n", "a\na"),
    ] {
        std::fs::write(&path, file).unwrap();
        let r = run(
            &EditTool,
            dir,
            json!({ "path": path, "old_string": old_string, "new_string": "z" }),
        )
        .await;
        assert!(out(&r).contains("appears more than once"));
        assert_eq!(read(&path), file);
    }
}

#[tokio::test]
async fn edit_reads_output_is_taken_as_copied_a_crlf_files_crs_too() {
    let d = sandbox();
    let dir = d.path();
    let path = s(&dir.join("copied.ts"));
    std::fs::write(&path, "function f() {\r\n  return 1;\r\n}\r\n").unwrap();
    run(
        &EditTool,
        dir,
        json!({ "path": path, "old_string": "     1\tfunction f() {\r\n     2\t  return 1;\r",
            "new_string": "     1\tfunction f() {\n     2\t  return 2;" }),
    )
    .await;
    assert_eq!(read(&path), "function f() {\r\n  return 2;\r\n}\r\n");
    // A last line of only indentation is where the next line starts, so that line keeps its own.
    std::fs::write(&path, "foo \n  next\n").unwrap();
    run(
        &EditTool,
        dir,
        json!({ "path": path, "old_string": "foo\n  ", "new_string": "bar\n  " }),
    )
    .await;
    assert_eq!(read(&path), "bar\n  next\n");
}

#[tokio::test]
async fn edit_a_line_of_only_digits_among_reads_line_numbers_is_refused_as_ambiguous() {
    let d = sandbox();
    let dir = d.path();
    let path = s(&dir.join("digits.txt"));
    let imports = format!("{}import a\nimport b\n\nfn main() {{}}\n", "\n".repeat(9));
    let cases: [(&str, &str, &str); 13] = [
        // A blank line's number without its tab: deleting the line before it, the same in Python, inserting without renumbering.
        (
            &imports,
            "    10\timport a\n    11\timport b\n    12\n    13\tfn main() {}",
            "    10\timport a\n    12\n    13\tfn main() {}",
        ),
        (
            "def f():\n    x = 1\n    y = 2\n\n    return x\n",
            "     2\t    x = 1\n     3\t    y = 2\n     4\n     5\t    return x",
            "     2\t    x = 1\n     4\n     5\t    return x",
        ),
        (
            "a\n\nb\n",
            "     1\ta\n     2\n     3\tb",
            "     1\ta\n     2\tins\n     2\n     3\tb",
        ),
        (
            "function f() {\n\n  return 1;\n}\n",
            "     1\tfunction f() {\n     2\n     3\t  return 1;",
            "     1\tfunction f() {\n     2\n     3\t  return 2;",
        ),
        (
            "a\n\n\nb\n",
            "     1\ta\n     2\n     3\n     4\tb",
            "     1\ta\n     2\n     3\tc\n     4\tb",
        ),
        // Text of only digits: a line added without a number, at any width, a new_string written without numbers, and old_string's own.
        (
            "let v = [\n  1,\n  2\n];\n",
            "     2\t  1,\n     3\t  2",
            "     2\t  1,\n     3\t  2,\n  3",
        ),
        (
            "let v = [\n  1,\n  2\n];\n",
            "     2\t  1,\n     3\t  2",
            "     2\t  1,\n     3\t  2,\n  1000",
        ),
        (
            "fn f() -> i32 {\n    1\n}\n",
            "     1\tfn f() -> i32 {\n     2\t    1\n     3\t}",
            "     1\tfn f() -> i32 {\n    0\n     3\t}",
        ),
        (
            "fn f() -> i32 {\n    1\n}\n",
            "     1\tfn f() -> i32 {\n     2\t    1\n     3\t}",
            "     1\tfn f() -> i32 {\n    42\n     3\t}",
        ),
        ("a\nb\n", "     1\ta\n     2\tb", "     1\ta\n     2\tb\n100000"),
        ("a\nb\n", "     1\ta", "0\na"),
        ("foo\n42\n", "     1\tfoo\n42", "     1\tbar\n43"),
        ("foo\n    42\n", "     1\tfoo\n    42", "     1\tbar\n    43"),
    ];
    for (file, old_string, new_string) in cases {
        std::fs::write(&path, file).unwrap();
        let r = run(
            &EditTool,
            dir,
            json!({ "path": path, "old_string": old_string, "new_string": new_string }),
        )
        .await;
        assert_eq!(r.is_error, Some(true));
        assert!(
            out(&r).contains("line-number form is ambiguous there"),
            "{old_string:?}: {}",
            out(&r)
        );
        assert_eq!(read(&path), file);
    }
    // With the tab read gives a blank line, the same edits are made as meant.
    std::fs::write(&path, &imports).unwrap();
    run(
        &EditTool,
        dir,
        json!({ "path": path, "old_string": "    10\timport a\n    11\timport b\n    12\t\n    13\tfn main() {}",
            "new_string": "    10\timport a\n    12\t\n    13\tfn main() {}" }),
    )
    .await;
    assert_eq!(read(&path), format!("{}import a\n\nfn main() {{}}\n", "\n".repeat(9)));
    std::fs::write(&path, "a\n\nb\n").unwrap();
    run(
        &EditTool,
        dir,
        json!({ "path": path, "old_string": "     1\ta\n     2\t\n     3\tb",
            "new_string": "     1\ta\n     2\tins\n     2\t\n     3\tb" }),
    )
    .await;
    assert_eq!(read(&path), "a\nins\n\nb\n");
}

#[tokio::test]
async fn edit_a_loose_match_that_would_change_nothing_is_an_error_so_loop_guard_sees_no_change() {
    let d = sandbox();
    let dir = d.path();
    let path = s(&dir.join("same.txt"));
    std::fs::write(&path, "a\nb\n").unwrap();
    let r = run(
        &EditTool,
        dir,
        json!({ "path": path, "old_string": "     1\ta", "new_string": "a" }),
    )
    .await;
    assert_eq!(r.is_error, Some(true));
    assert!(out(&r).contains("identical"));
    assert!(r.details.is_none());
}

#[tokio::test]
async fn edit_an_old_string_that_starts_with_a_blank_line_is_found_in_time_next_to_a_long_run_of_spaces() {
    let d = sandbox();
    let dir = d.path();
    let path = s(&dir.join("padded.txt"));
    std::fs::write(&path, format!("x{}y\nfoo();\n  needle();\n", " ".repeat(100_000))).unwrap();
    let started = Instant::now();
    let r = run(
        &EditTool,
        dir,
        json!({ "path": path, "old_string": "\n    needle();", "new_string": "\n    pin();" }),
    )
    .await;
    assert!(started.elapsed() < Duration::from_millis(2000));
    assert!(!r.is_error());
    assert!(read(&path).ends_with("y\nfoo();\n  pin();\n"));
}

#[tokio::test]
async fn edit_a_long_loose_edit_that_adds_a_depth_old_string_lacks_is_mapped_in_time() {
    let d = sandbox();
    let dir = d.path();
    let path = s(&dir.join("long.ts"));
    let (mut file, mut old, mut neu) = (Vec::new(), Vec::new(), Vec::new());
    // Dedented by one tab, with a line two tabs deeper after each: the shift comes from old_string's two depths.
    for k in 0..300 {
        file.extend([format!("\ta{k}();"), format!("\t\tb{k}();")]);
        old.extend([format!("a{k}();"), format!("\tb{k}();")]);
        neu.extend([format!("a{k}();"), format!("\tb{k}();"), format!("\t\tc{k}();")]);
    }
    std::fs::write(&path, format!("{{\n{}\n}}\n", file.join("\n"))).unwrap();
    let started = Instant::now();
    let r = run(
        &EditTool,
        dir,
        json!({ "path": path, "old_string": old.join("\n"), "new_string": neu.join("\n") }),
    )
    .await;
    assert!(started.elapsed() < Duration::from_millis(2000));
    assert!(out(&r).contains("indentation]"));
    assert!(read(&path).starts_with("{\n\ta0();\n\t\tb0();\n\t\t\tc0();\n\ta1();\n"));
}

#[tokio::test]
async fn read_stops_on_a_whole_line_within_100000_characters_and_says_where_to_go_on() {
    let d = sandbox();
    let dir = d.path();
    let lines: Vec<String> = (0..2000)
        .map(|i| format!("{:x<200}", format!("line {} ", i + 1)))
        .collect();
    std::fs::write(dir.join("wide.txt"), format!("{}\n", lines.join("\n"))).unwrap();
    let first = out(&run(&ReadTool, dir, json!({ "path": "wide.txt" })).await);
    assert!(first.ends_with("\n[Showing lines 1-480 of 2000. Use offset=481 to continue.]"));
    let mut seen: Vec<String> = Vec::new();
    let mut offset = 1;
    while offset > 0 {
        let r = out(&run(&ReadTool, dir, json!({ "path": "wide.txt", "offset": offset })).await);
        let body = &r[..r.find("\n[Showing").unwrap_or(r.len())];
        assert!(body.chars().map(char::len_utf16).sum::<usize>() <= 100_000);
        seen.extend(
            body.split('\n')
                .map(|l| l.trim_start_matches(' ').split_once('\t').unwrap().1.to_string()),
        );
        offset = r
            .find("Use offset=")
            .map(|i| {
                r[i + 11..]
                    .chars()
                    .take_while(char::is_ascii_digit)
                    .collect::<String>()
                    .parse()
                    .unwrap()
            })
            .unwrap_or(0);
    }
    assert_eq!(seen, lines);
    // Long lines are cut first, and only the ones shown are counted.
    std::fs::write(dir.join("long.txt"), format!("{}\n", "y".repeat(3000)).repeat(60)).unwrap();
    let long = out(&run(&ReadTool, dir, json!({ "path": "long.txt" })).await);
    assert!(long.contains("[Showing lines 1-49 of 60. Use offset=50 to continue.]\n[49 long line(s) truncated"));
}

#[tokio::test]
async fn edit_refuses_a_file_that_is_not_utf8_and_leaves_its_bytes_as_they_were_read_says_so() {
    let d = sandbox();
    let dir = d.path();
    // Shift_JIS for "// 設定\nconst x = 1;\n// 終わり\n"
    let sjis = hex("2f2f2090dd92e80a636f6e73742078203d20313b0a2f2f208f4982ed82e80a");
    std::fs::write(dir.join("sjis.js"), &sjis).unwrap();
    let r = run(
        &EditTool,
        dir,
        json!({ "path": "sjis.js", "old_string": "const x = 1;", "new_string": "const x = 2;" }),
    )
    .await;
    assert_eq!(r.is_error, Some(true));
    assert!(out(&r).contains("not valid UTF-8"));
    assert_eq!(std::fs::read(dir.join("sjis.js")).unwrap(), sjis);
    assert!(out(&run(&ReadTool, dir, json!({ "path": "sjis.js" })).await).contains("is not valid UTF-8"));
    std::fs::write(dir.join("utf8.js"), "// 設定\nconst x = 1;\n").unwrap();
    assert!(!out(&run(&ReadTool, dir, json!({ "path": "utf8.js" })).await).contains("UTF-8"));
    let r = run(
        &EditTool,
        dir,
        json!({ "path": "utf8.js", "old_string": "const x = 1;", "new_string": "const x = 2;" }),
    )
    .await;
    assert!(!r.is_error());
}

#[tokio::test]
async fn read_says_a_file_over_10_mb_is_not_utf8_too_by_its_first_lines() {
    let d = sandbox();
    let dir = d.path();
    let filler: Vec<u8> = b"a\n".iter().copied().cycle().take(10_000_001).collect();
    let sjis = hex("2f2f2090dd92e80a"); // "// 設定\n" in Shift_JIS
    std::fs::write(dir.join("big-sjis.txt"), [sjis, filler.clone()].concat()).unwrap();
    assert!(
        out(&run(&ReadTool, dir, json!({ "path": "big-sjis.txt", "limit": 3 })).await).contains("is not valid UTF-8")
    );
    std::fs::write(
        dir.join("big-utf8.txt"),
        ["// 設定\n".as_bytes().to_vec(), filler].concat(),
    )
    .unwrap();
    assert!(!out(&run(&ReadTool, dir, json!({ "path": "big-utf8.txt", "limit": 3 })).await).contains("UTF-8"));
}

#[tokio::test]
async fn read_and_bash_never_cut_a_surrogate_pair_in_half() {
    if !in_sandboxed_home("read_and_bash_never_cut_a_surrogate_pair_in_half") {
        return;
    }
    // A Rust String cannot hold half a pair, so `isWellFormed` is true by construction; what is
    // left to check is where the cut falls.
    let d = sandbox();
    let dir = d.path();
    std::fs::write(
        dir.join("emoji.txt"),
        format!("{}{}\n", "x".repeat(1999), "😀".repeat(50)),
    )
    .unwrap();
    let r = out(&run(&ReadTool, dir, json!({ "path": "emoji.txt" })).await);
    assert!(r.contains(&format!("\t{}… [line truncated, 2099 chars]", "x".repeat(1999))));
    let b = out(&run(
        &BashTool,
        dir,
        json!({ "command": "for i in $(seq 1 20000); do printf '😀'; done; echo; echo o" }),
    )
    .await);
    // 40,003 units in all; the tail is the last whole characters within 30,000: "\no\n" and 14,998
    // emoji (the TS, run with bun, gives the same).
    assert!(b.starts_with(&format!(
        "{}\no\n[Output truncated: showing the last 30000 of 40003 characters.",
        "😀".repeat(14_998)
    )));
}

#[tokio::test]
async fn bash_a_timeout_past_what_a_timer_holds_is_capped_at_24_hours_not_fired_at_once() {
    let d = sandbox();
    let r = run(
        &BashTool,
        d.path(),
        json!({ "command": "sleep 0.3; echo done", "timeout": 3_600_000 }),
    )
    .await;
    assert_eq!(out(&r), "done\n[exit code 0]");
    assert_eq!(BashTool.parameters()["properties"]["timeout"]["maximum"], 86_400);
}

fn hex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

// ── New: what the model, the TUI and the permission system see ─────────

/// `{name, description, parameters, kind, alwaysLoad, concurrent}` of each tool as the TS dumped
/// them (bun, packages/tools/src/index.ts, on macOS): descriptions and schemas byte for byte.
const TS_DEFINITIONS: &str = r#"{"name":"read","description":"Read a file. Text is returned with line numbers (cat -n style), up to 2000 lines or 100000 characters per call; use offset/limit for more. Images (png, jpg, gif, webp) are returned as images.","parameters":{"type":"object","properties":{"path":{"type":"string","description":"File path, absolute or relative to the working directory"},"offset":{"type":"integer","minimum":1,"description":"1-based line to start from"},"limit":{"type":"integer","minimum":1,"description":"Maximum number of lines"}},"required":["path"],"additionalProperties":false},"kind":"read","alwaysLoad":true,"concurrent":true}
{"name":"write","description":"Create a file or overwrite it entirely. Parent directories are created as needed. Prefer edit for changes to existing files.","parameters":{"type":"object","properties":{"path":{"type":"string","description":"File path, absolute or relative to the working directory"},"content":{"type":"string","description":"Full file contents"}},"required":["path","content"],"additionalProperties":false},"kind":"edit","alwaysLoad":true}
{"name":"edit","description":"Replace an exact string in a file. old_string must match the file exactly (including whitespace) and be unique unless replace_all is true.","parameters":{"type":"object","properties":{"path":{"type":"string","description":"File path, absolute or relative to the working directory"},"old_string":{"type":"string","description":"Exact text to replace"},"new_string":{"type":"string","description":"Replacement text"},"replace_all":{"type":"boolean","description":"Replace every occurrence (default false)"}},"required":["path","old_string","new_string"],"additionalProperties":false},"kind":"edit","alwaysLoad":true}
{"name":"bash","description":"Run a shell command with bash in the working directory. Returns combined stdout/stderr and the exit code. Output over 30000 characters is cut to the tail and saved to a file whose path is returned. Use ripgrep (rg) for searching.","parameters":{"type":"object","properties":{"command":{"type":"string","description":"Command to run"},"timeout":{"type":"number","maximum":86400,"description":"Timeout in seconds (default 120, at most 86400)"}},"required":["command"],"additionalProperties":false},"kind":"exec","alwaysLoad":true}"#;

#[test]
fn definitions_match_the_ts_byte_for_byte() {
    let tools = builtin_tools();
    let lines: Vec<&str> = TS_DEFINITIONS.lines().collect();
    assert_eq!(tools.len(), lines.len());
    for (t, line) in tools.iter().zip(lines) {
        let mut d = serde_json::Map::new();
        d.insert("name".into(), json!(t.name()));
        d.insert("description".into(), json!(t.description()));
        d.insert("parameters".into(), t.parameters());
        d.insert("kind".into(), serde_json::to_value(t.kind()).unwrap());
        d.insert("alwaysLoad".into(), json!(t.always_load()));
        if t.concurrent() {
            d.insert("concurrent".into(), json!(true));
        }
        if cfg!(windows) && t.name() == "bash" {
            continue; // the Windows note is appended there
        }
        assert_eq!(Value::Object(d).to_string(), line);
    }
}

#[test]
fn summaries_match_targets_and_paths() {
    let cwd = Path::new("/w");
    let a = |v: Value| args(v);
    assert_eq!(
        ReadTool.summary(&a(json!({ "path": "a", "offset": 3 }))).as_deref(),
        Some("a:3")
    );
    assert_eq!(
        ReadTool.summary(&a(json!({ "path": "a", "offset": 0 }))).as_deref(),
        Some("a")
    );
    assert_eq!(
        BashTool.match_target(&a(json!({ "command": "ls" }))).as_deref(),
        Some("ls")
    );
    assert_eq!(BashTool.summary(&a(json!({ "command": "ls" }))).as_deref(), Some("ls"));
    // On Windows `/w` is on the working directory's drive.
    let written = if cfg!(windows) {
        let here = std::env::current_dir().unwrap().display().to_string();
        format!("{}\\w\\b\\c", &here[..2])
    } else {
        "/w/b/c".to_string()
    };
    assert_eq!(
        WriteTool.paths(&a(json!({ "path": "a/../b/./c/" })), cwd),
        Some(vec![written])
    );
    assert_eq!(EditTool.summary(&a(json!({ "path": "x.rs" }))).as_deref(), Some("x.rs"));
    assert!(ReadTool.paths(&a(json!({ "path": "/abs" })), cwd).is_some());
}

#[tokio::test]
async fn results_and_errors_as_the_ts_wrote_them() {
    let d = sandbox();
    let dir = d.path();
    // Values from running the TS tools (bun) on the same inputs.
    std::fs::create_dir(dir.join("adir")).unwrap();
    std::fs::write(dir.join("afile"), "x").unwrap();
    let e = WriteTool
        .execute(args(json!({ "path": "adir", "content": "x" })), ctx(dir))
        .await
        .unwrap_err();
    assert_eq!(
        e.to_string(),
        format!(
            "EISDIR: illegal operation on a directory, open '{}'",
            s(&dir.join("adir"))
        )
    );
    let e = WriteTool
        .execute(args(json!({ "path": "afile/sub/x.txt", "content": "x" })), ctx(dir))
        .await
        .unwrap_err();
    assert_eq!(
        e.to_string(),
        format!(
            "ENOTDIR: not a directory, mkdir '{}'",
            s(&dir.join("afile").join("sub"))
        )
    );
    let r = run(
        &EditTool,
        dir,
        json!({ "path": "adir", "old_string": "b", "new_string": "B" }),
    )
    .await;
    assert_eq!(
        out(&r),
        format!("File not found: {}. Use write to create it.", s(&dir.join("adir")))
    );
    let r = run(&BashTool, &dir.join("missing"), json!({ "command": "echo hi" })).await;
    // `bash` is Git Bash's full path on Windows.
    let bash = sasacode_tools::shell::bash_path().unwrap();
    assert_eq!(
        out(&r),
        format!("Error: ENOENT: no such file or directory, posix_spawn '{bash}'\n[terminated by null]")
    );
    assert_eq!(
        serde_json::to_string(r.details.as_ref().unwrap()).unwrap(),
        r#"{"exitCode":null}"#
    );
    std::fs::write(dir.join("empty.txt"), "").unwrap();
    let r = run(&ReadTool, dir, json!({ "path": "empty.txt", "offset": 2 })).await;
    assert_eq!(out(&r), "offset 2 is past the end of the file (1 lines).");
    let r = run(&ReadTool, dir, json!({ "path": "empty.txt" })).await;
    assert_eq!(out(&r), format!("{} is empty.", s(&dir.join("empty.txt"))));
    assert_eq!(r.is_error, None);
    let r = run(&BashTool, dir, json!({ "command": "kill -SEGV $$" })).await;
    if cfg!(windows) {
        // No signals on Windows: Git Bash's emulated one ends the process with an exit code.
        assert!(out(&r).starts_with("[exit code "), "{}", out(&r));
        assert_eq!(r.is_error, Some(true));
    } else {
        assert_eq!(out(&r), "[terminated by SIGSEGV]");
        assert_eq!(
            serde_json::to_string(&r).unwrap(),
            r#"{"content":[{"type":"text","text":"[terminated by SIGSEGV]"}],"isError":true,"details":{"exitCode":null}}"#
        );
    }
    let r = run(&BashTool, dir, json!({ "command": "echo out; echo err >&2; exit 0" })).await;
    assert_eq!(
        serde_json::to_string(&r).unwrap(),
        r#"{"content":[{"type":"text","text":"out\nerr\n[exit code 0]"}],"isError":false,"details":{"exitCode":0}}"#
    );
    let r = run(&BashTool, dir, json!({ "command": "true" })).await;
    assert_eq!(out(&r), "[exit code 0]");
    // A write's details, diff included.
    let r = run(&WriteTool, dir, json!({ "path": "w.txt", "content": "a\nb\n" })).await;
    // As written inside a JSON string (`\` doubled on Windows).
    let p = serde_json::to_string(&s(&dir.join("w.txt"))).unwrap();
    let p = &p[1..p.len() - 1];
    assert_eq!(
        serde_json::to_string(&r).unwrap(),
        format!(
            r#"{{"content":[{{"type":"text","text":"Created {p} (2 lines)."}}],"details":{{"path":"{p}","changed":true,"created":true}}}}"#
        )
    );
    let r = run(&WriteTool, dir, json!({ "path": "w.txt", "content": "a\nc\n" })).await;
    assert_eq!(
        serde_json::to_string(r.details.as_ref().unwrap()).unwrap(),
        format!(r#"{{"path":"{p}","changed":true,"created":false,"diff":"@@ -1 +1 @@\n a\n-b\n+c\n "}}"#)
    );
}

#[tokio::test]
async fn bash_streams_partial_output_and_large_files_split_lines_as_readline_does() {
    let d = sandbox();
    let dir = d.path();
    let seen = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let mut c = ctx(dir);
    let sink = seen.clone();
    c.on_update = Some(std::sync::Arc::new(move |s: String| sink.lock().unwrap().push_str(&s)));
    let r = BashTool
        .execute(args(json!({ "command": "echo one; sleep 0.1; echo two" })), c)
        .await
        .unwrap();
    assert_eq!(out(&r), "one\ntwo\n[exit code 0]");
    assert_eq!(*seen.lock().unwrap(), "one\ntwo\n");
    // Over 10 MB, lines end at \r\n, \n, a lone \r, U+2028 and U+2029, as in the TS (bun, readline).
    let filler: Vec<u8> = b"x\n".iter().copied().cycle().take(10_000_001).collect();
    std::fs::write(
        dir.join("big-cr.txt"),
        ["a\r\nb\rc\n\rd\r\r\ne\u{2028}f\n".as_bytes().to_vec(), filler].concat(),
    )
    .unwrap();
    let r = out(&run(&ReadTool, dir, json!({ "path": "big-cr.txt", "limit": 7 })).await);
    assert_eq!(
        r,
        "     1\ta\n     2\tb\n     3\tc\n     4\t\n     5\td\n     6\t\n     7\te\n[Showing lines 1-7 of a 10 MB file. Use offset=8 to continue.]"
    );
    let mut end: Vec<u8> = b"z\n".iter().copied().cycle().take(10_000_001).collect();
    end.push(b'\r');
    std::fs::write(dir.join("big-end.txt"), end).unwrap();
    let r = out(&run(
        &ReadTool,
        dir,
        json!({ "path": "big-end.txt", "offset": 5_000_000, "limit": 3 }),
    )
    .await);
    assert_eq!(r, "5000000\tz\n5000001\tz\n[Showing lines 5000000-5000001 of 5000001.]");
}

#[tokio::test]
async fn bom_and_killed_commands_as_the_ts_wrote_them() {
    // Values from running the TS tools (bun) on the same inputs: a UTF-8 BOM is text like any other.
    let d = sandbox();
    let dir = d.path();
    let bom = "\u{FEFF}";
    std::fs::write(dir.join("b.txt"), format!("{bom}x\ny\n")).unwrap();
    assert_eq!(
        out(&run(&ReadTool, dir, json!({ "path": "b.txt" })).await),
        format!("     1\t{bom}x\n     2\ty")
    );
    let mut big = format!("{bom}x\ny\n").into_bytes();
    big.extend(b"z\n".iter().copied().cycle().take(10_000_001));
    std::fs::write(dir.join("big.txt"), big).unwrap();
    assert_eq!(
        out(&run(&ReadTool, dir, json!({ "path": "big.txt", "limit": 2 })).await),
        format!("     1\t{bom}x\n     2\ty\n[Showing lines 1-2 of a 10 MB file. Use offset=3 to continue.]")
    );
    let r = run(&WriteTool, dir, json!({ "path": "b.txt", "content": "x\ny\n" })).await;
    assert_eq!(
        r.details.as_ref().unwrap()["diff"],
        format!("@@ -1 +1 @@\n-{bom}x\n+x\n y\n ")
    );
    std::fs::write(dir.join("b.txt"), format!("{bom}x\ny\n")).unwrap();
    let r = run(
        &WriteTool,
        dir,
        json!({ "path": "b.txt", "content": format!("{bom}x\ny\n") }),
    )
    .await;
    assert_eq!(r.details.as_ref().unwrap()["changed"], false);
    let r = run(
        &EditTool,
        dir,
        json!({ "path": "b.txt", "old_string": "y", "new_string": "Y" }),
    )
    .await;
    assert_eq!(
        r.details.as_ref().unwrap()["diff"],
        format!("@@ -1 +1 @@\n {bom}x\n-y\n+Y\n ")
    );
    assert_eq!(std::fs::read(dir.join("b.txt")).unwrap(), b"\xef\xbb\xbfx\nY\n");
    let r = run(&BashTool, dir, json!({ "command": "printf '\\357\\273\\277x'" })).await;
    assert_eq!(out(&r), format!("{bom}x\n[exit code 0]"));
    // Killed commands: the whole result. Windows has no signals: taskkill /F ends the tree, which
    // reads as exit code 1 (as in the TS).
    let json_of = |r: &ToolResult| serde_json::to_string(r).unwrap();
    let killed = |text: &str, signal: &str, by: &str| {
        let (note, code) = if cfg!(windows) {
            ("[exit code 1]".to_string(), "1")
        } else {
            (format!("[terminated by {signal}]"), "null")
        };
        format!(
            r#"{{"content":[{{"type":"text","text":"{text}{note}"}}],"isError":true,"details":{{"exitCode":{code},"killedBy":"{by}"}}}}"#
        )
    };
    let r = run(&BashTool, dir, json!({ "command": "sleep 5", "timeout": 0.3 })).await;
    assert_eq!(
        json_of(&r),
        killed(r"[Command timed out after 0.3s and was killed]\n", "SIGTERM", "timeout")
    );
    // SIGTERM ignored: SIGKILL 2 s later.
    let r = run(
        &BashTool,
        dir,
        json!({ "command": "trap '' TERM; echo started; sleep 3", "timeout": 0.3 }),
    )
    .await;
    assert_eq!(
        json_of(&r),
        killed(
            r"started\n[Command timed out after 0.3s and was killed]\n",
            "SIGKILL",
            "timeout"
        )
    );
    let c = ctx(dir);
    let signal = c.signal.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        signal.cancel();
    });
    let r = BashTool
        .execute(
            args(json!({ "command": "echo before; sleep 30 & sleep 30; echo done" })),
            c,
        )
        .await
        .unwrap();
    assert_eq!(
        json_of(&r),
        killed(r"before\n[Command was interrupted by the user]\n", "SIGTERM", "abort")
    );
    let c = ctx(dir);
    c.signal.cancel();
    let r = BashTool.execute(args(json!({ "command": "echo x" })), c).await.unwrap();
    assert_eq!(
        json_of(&r),
        r#"{"content":[{"type":"text","text":"[Command was interrupted before execution]"}],"isError":true}"#
    );
}

/// Node's `detached: true`: bash leads a session of its own, with no controlling terminal, so
/// prompts that open /dev/tty fail at once (rust/DECISIONS.md ADR-012).
#[cfg(unix)]
#[tokio::test]
async fn bash_runs_in_a_session_of_its_own() {
    if std::process::Command::new("python3")
        .arg("-c")
        .arg("0")
        .output()
        .is_err()
    {
        return;
    }
    let dir = sandbox();
    let r = run(
        &BashTool,
        dir.path(),
        json!({ "command": "python3 -c 'import os; print(\"leader\" if os.getsid(0) in (os.getpid(), os.getppid()) else \"not\")'" }),
    )
    .await;
    // bash may exec python directly, so the leader is python itself or its parent bash.
    assert_eq!(out(&r), "leader\n[exit code 0]");
}
