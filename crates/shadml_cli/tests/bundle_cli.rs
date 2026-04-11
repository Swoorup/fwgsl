use std::fs;

use assert_cmd::Command;
use tempfile::tempdir;

#[test]
fn bundle_reports_name_collisions_with_file_context() {
    let temp = tempdir().expect("should create temp dir");
    let root = temp.path();

    fs::write(
        root.join("Main.shadml"),
        "import A\nimport B\n\nuse_helper : I32 -> I32\nuse_helper x = helper x\n",
    )
    .expect("should write main module");
    fs::write(
        root.join("A.shadml"),
        "module A\n\nhelper : I32 -> I32\nhelper x = x + 1\n",
    )
    .expect("should write module A");
    fs::write(
        root.join("B.shadml"),
        "module B\n\nhelper : I32 -> I32\nhelper x = x + 2\n",
    )
    .expect("should write module B");

    let output = Command::cargo_bin("shadml")
        .expect("should build shadml binary")
        .current_dir(root)
        .arg("bundle")
        .arg("Main.shadml")
        .output()
        .expect("should run shadml bundle");

    assert!(
        !output.status.success(),
        "bundle should fail on duplicate exported names"
    );

    let stderr = String::from_utf8(output.stderr).expect("stderr should be utf8");
    assert!(stderr.contains("name collisions across modules"));
    assert!(stderr.contains("function 'helper' defined in:"));
    assert!(stderr.contains("A ("));
    assert!(stderr.contains("A.shadml"));
    assert!(stderr.contains("B ("));
    assert!(stderr.contains("B.shadml"));
}
