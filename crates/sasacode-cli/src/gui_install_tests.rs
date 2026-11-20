use super::*;

fn repo() -> PathBuf {
    std::path::absolute(Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")).unwrap()
}

/// An empty program named cargo in `dir` (which only looks for an executable file).
fn fake_cargo(dir: &Path) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let cargo = dir.join(format!("cargo{}", std::env::consts::EXE_SUFFIX));
    std::fs::write(&cargo, "").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&cargo, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    cargo
}

#[test]
fn without_path_or_commit_the_source_is_this_versions_tag_of_the_repository() {
    let tag = Source::Tag {
        git: REPO.to_string(),
        tag: format!("v{VERSION}"),
    };
    assert_eq!(source_for_commit(None, None, None).unwrap(), tag);
    assert_eq!(source_for_commit(None, Some(""), Some("")).unwrap(), tag);
    assert_eq!(
        source_for_commit(None, Some("file:///mirror"), None).unwrap(),
        Source::Tag {
            git: "file:///mirror".to_string(),
            tag: format!("v{VERSION}")
        }
    );
}

#[test]
fn a_release_builds_the_window_from_the_commit_it_was_built_from() {
    let rev = "0123456789abcdef0123456789ABCDEF01234567";
    assert_eq!(
        source_for_commit(None, None, Some(rev)).unwrap(),
        Source::Commit {
            git: REPO.to_string(),
            rev: rev.to_ascii_lowercase()
        }
    );
    assert_eq!(
        source_for_commit(None, Some("file:///mirror"), Some(rev)).unwrap(),
        Source::Commit {
            git: "file:///mirror".to_string(),
            rev: rev.to_ascii_lowercase()
        }
    );
    // Not a full hash (a branch, a tag, an abbreviation): never handed to cargo as one.
    for bad in ["main", "v0.20.0", "0123456", "0123456789abcdef0123456789abcdef0123456z"] {
        let e = source_for_commit(None, None, Some(bad)).unwrap_err().to_string();
        assert!(e.contains(bad) && e.contains("--path"), "{e}");
    }
    // --path wins: a checkout is built as it is.
    assert!(matches!(
        source_for_commit(Some(&repo()), None, Some(rev)).unwrap(),
        Source::Checkout(_)
    ));
}

#[test]
fn path_takes_the_repository_or_its_gui_crate_and_nothing_else() {
    let gui = repo().join("crates").join("sasacode-gui");
    assert_eq!(source_for(Some(&repo()), None).unwrap(), Source::Checkout(gui.clone()));
    assert_eq!(source_for(Some(&gui), None).unwrap(), Source::Checkout(gui.clone()));
    let elsewhere = tempfile::tempdir().unwrap();
    let e = source_for(Some(elsewhere.path()), None).unwrap_err().to_string();
    assert!(e.contains("crates/sasacode-gui"), "{e}");
    // The CLI's own crate is a crate, not the window's.
    let cli = repo().join("crates").join("sasacode-cli");
    assert!(source_for(Some(&cli), None).is_err());
}

#[test]
fn cargo_args_build_locked_into_the_root_from_the_tag_or_the_checkout() {
    let root = Path::new("/home/u/.sasacode/gui");
    let tag = Source::Tag {
        git: REPO.to_string(),
        tag: "v0.20.0".to_string(),
    };
    let args: Vec<String> = cargo_args(&tag, root)
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let head = ["install", "--locked", "--force", "--root", "/home/u/.sasacode/gui"];
    assert_eq!(args[..5], head);
    assert_eq!(args[5..], ["--git", REPO, "--tag", "v0.20.0", "sasacode-gui"]);
    let commit = Source::Commit {
        git: REPO.to_string(),
        rev: "0123456789abcdef0123456789abcdef01234567".to_string(),
    };
    let args: Vec<String> = cargo_args(&commit, root)
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    assert_eq!(args[..5], head);
    assert_eq!(
        args[5..],
        [
            "--git",
            REPO,
            "--rev",
            "0123456789abcdef0123456789abcdef01234567",
            "sasacode-gui"
        ]
    );
    let checkout = Source::Checkout(PathBuf::from("/src/sasacode/crates/sasacode-gui"));
    let args: Vec<String> = cargo_args(&checkout, root)
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    assert_eq!(args[..5], head);
    assert_eq!(args[5..], ["--path", "/src/sasacode/crates/sasacode-gui"]);
}

#[test]
fn cargo_is_found_on_path_then_in_cargo_home_then_in_dot_cargo() {
    let t = tempfile::tempdir().unwrap();
    let cwd = t.path();
    let on_path = fake_cargo(&t.path().join("path"));
    let in_cargo_home = fake_cargo(&t.path().join("cargo-home").join("bin"));
    let in_home = fake_cargo(&t.path().join("home").join(".cargo").join("bin"));
    let path = Some(OsString::from(t.path().join("path")));
    let empty = Some(OsString::from(t.path().join("nothing-here")));
    let cargo_home = Some(t.path().join("cargo-home"));
    let home = Some(t.path().join("home"));
    assert_eq!(
        find_cargo_in(path, cargo_home.clone(), home.clone(), cwd),
        Some(on_path)
    );
    assert_eq!(
        find_cargo_in(empty.clone(), cargo_home, home.clone(), cwd),
        Some(in_cargo_home)
    );
    assert_eq!(find_cargo_in(empty.clone(), None, home, cwd), Some(in_home));
    assert_eq!(
        find_cargo_in(
            empty.clone(),
            Some(t.path().join("none")),
            Some(t.path().join("none")),
            cwd
        ),
        None
    );
    assert_eq!(find_cargo_in(None, None, None, cwd), None);
}

#[test]
fn without_rust_the_way_to_get_it_is_said() {
    let said = no_rust();
    assert!(said.contains("https://rustup.rs"), "{said}");
    assert!(said.contains("sasacode gui install"), "{said}");
}

#[test]
fn cargo_is_given_path_with_the_install_folder_first() {
    let dir = Path::new("/home/u/.sasacode/gui/bin");
    let (key, value) = path_with(dir).unwrap();
    assert!(key.eq_ignore_ascii_case("PATH"), "{key}");
    let mut parts = std::env::split_paths(&value);
    assert_eq!(parts.next().as_deref(), Some(dir));
    assert!(parts.all(|p| !p.as_os_str().is_empty()), "{value}");
}
