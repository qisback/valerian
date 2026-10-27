// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! End-to-end tests of the `valerian` binary.

use assert_cmd::Command;
use predicates::prelude::*;

fn valerian() -> Command {
    Command::cargo_bin("valerian").expect("valerian binary")
}

#[test]
fn short_version_is_one_line() {
    valerian()
        .arg("-V")
        .assert()
        .success()
        .stdout(predicate::eq("valerian 0.5.0\n"));
}

#[test]
fn long_version_has_notices_and_dedication() {
    valerian()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::starts_with("valerian 0.5.0\n"))
        .stdout(predicate::str::contains(
            "Copyright (C) 2026 Laurence Horrocks-Barlow",
        ))
        .stdout(predicate::str::contains("WITHOUT ANY WARRANTY"))
        .stdout(predicate::str::contains("ADDITIONAL-TERMS.md"))
        .stdout(predicate::str::contains(
            "Valerian is dedicated to my son Xaviar.",
        ))
        .stdout(predicate::str::contains("https://12hobbies.studio"));
}

#[test]
fn info_prints_the_build_record() {
    valerian()
        .arg("info")
        .assert()
        .success()
        .stdout(predicate::str::contains("commit: "))
        .stdout(predicate::str::contains("wgpu: "));
}

#[test]
fn unknown_subcommand_fails() {
    valerian().arg("nonsense").assert().failure();
}
