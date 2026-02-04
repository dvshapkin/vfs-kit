use vfs_kit::{DirFS, FsBackend};

fn main() {
    let tmp = std::env::temp_dir();
    println!("Temp dir: {}", tmp.display());

    let root = tmp.join("my_vfs");

    // creates `/tmp/my_vfs` on host and remember `my_vfs` as created;
    // set inner root to /tmp/my_vfs`;
    // set inner CWD (Current Working Dir) to `/`
    let mut fs = DirFS::new(root).unwrap();

    // creates `/tmp/my_vfs/docs` on host and remember `/docs` as created
    fs.mkdir("/docs").unwrap();

    // change inner CWD to `/docs`
    fs.cd("docs").unwrap();

    // creates file `/tmp/my_vfs/docs/first.txt` on host and remember it as created;
    // file `first.txt` will be created in CWD because filename is relative
    fs.mkfile("first.txt", Some(b"Hello")).unwrap();
    assert!(fs.exists("first.txt"));

    // creates file `/tmp/my_vfs/second.txt` on host and remember it as created;
    // file `/second.txt` will be created in the root because filename is absolute
    fs.mkfile("/second.txt", Some(b"World")).unwrap();
    assert!(fs.exists("/second.txt"));

    // change inner CWD to `/`
    fs.cd("..").unwrap();

    // reads content of the first file
    let first_content = fs.read("/docs/first.txt").unwrap();
    assert_eq!(first_content, b"Hello");

    // reads content of the second file
    let second_content = fs.read("/second.txt").unwrap();
    assert_eq!(second_content, b"World");

    println!(
        "{}, {}!",
        String::from_utf8(first_content).unwrap(),
        String::from_utf8(second_content).unwrap()
    );

    // removes both files
    fs.rm("/docs/first.txt").unwrap();
    fs.rm("/second.txt").unwrap();

    // At this point, the `fs` variable will be destroyed,
    // and all created and remembered artifacts (directories, files)
    // will also be deleted...
    // If you don't want them to be deleted, use set_auto_clean(false)
}
