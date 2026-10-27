// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Standard output that ends quietly when the reader goes away.
//!
//! Rust ignores `SIGPIPE`, so `println!` panics on `valerian info | head` once
//! `head` has exited. These macros shadow `print!` and `println!` in this
//! crate: a closed pipe ends the process with status 0 and no message (what a
//! shell user expects from `head`); any other write error is reported plainly.

use std::io::{self, Write};

/// Write `args` to standard output; see the module text for what a failure does.
pub fn write_stdout(args: std::fmt::Arguments<'_>, newline: bool) {
    let mut out = io::stdout().lock();
    let r = out.write_fmt(args).and_then(|()| {
        if newline {
            out.write_all(b"\n")?;
        }
        Ok(())
    });
    if let Err(e) = r {
        end_on(&e);
    }
}

/// Leave quietly for a closed pipe, loudly for anything else.
fn end_on(e: &io::Error) -> ! {
    if e.kind() == io::ErrorKind::BrokenPipe {
        std::process::exit(0);
    }
    eprintln!("valerian: cannot write to standard output: {e}");
    std::process::exit(1);
}

macro_rules! println {
    () => { $crate::out::write_stdout(format_args!(""), true) };
    ($($arg:tt)*) => { $crate::out::write_stdout(format_args!($($arg)*), true) };
}

macro_rules! print {
    ($($arg:tt)*) => { $crate::out::write_stdout(format_args!($($arg)*), false) };
}
