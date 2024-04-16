use std::{
	fs,
	path::{Path, PathBuf},
	sync::{
		atomic::{AtomicBool, Ordering},
		Arc,
	},
	thread,
};

use signal_hook::{
	consts::{SIGINT, SIGTERM},
	iterator::Signals,
};

use notify::{event::AccessKind, Watcher};

use clap::Parser;
use const_format::formatcp;
use lazy_static::lazy_static;
use regex::Regex;

const YEAR: &str = "YEAR";
const MONTH: &str = "MONTH";
const DAY: &str = "DAY";

const LATEST: &str = "latest";
const OTHER: &str = "other";

const NAME_REGEX_STR: &str = formatcp!(r"(?<{}>\d\d\d\d)-(?<{}>\d\d)-(?<{}>\d\d).*\.png$", YEAR, MONTH, DAY);

lazy_static! {
	static ref NAME_REGEX: Regex = Regex::new(NAME_REGEX_STR).unwrap();
}

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
#[command(propagate_version = true)]
pub struct Args {
	/// Path to screenshot directory
	#[arg(value_name = "PATH")]
	screenshot_dir: String,
}

fn check_exists(path: &Path) -> bool {
	let existence = path.try_exists();

	if let Err(e) = existence {
		eprintln!("Existence of \"{}\" could not be verified: {}", path.display(), e);

		false
	} else {
		let res = existence.unwrap();

		if !res {
			eprintln!("Path \"{}\" does not exist", path.display());
		}

		res
	}
}

fn update_file(path: &Path, file: &Path) -> anyhow::Result<()> {
	if !file.is_file() {
		return Ok(());
	}

	let filename = file.file_name().unwrap(); // Already checked
	let filename_lossy = filename.to_string_lossy();
	let matches = NAME_REGEX.captures(&filename_lossy);

	match matches {
		Some(matches) => {
			let year = matches.name(YEAR).unwrap();
			let month = matches.name(MONTH).unwrap();
			let day = matches.name(DAY).unwrap();

			move_files(
				path,
				&PathBuf::from(filename),
				&PathBuf::new().join(year.as_str()).join(month.as_str()).join(day.as_str()),
			)
			.map_err(anyhow::Error::msg)?;
		}
		None => {
			move_files(path, &PathBuf::from(filename), &PathBuf::from(OTHER)).map_err(anyhow::Error::msg)?;
		}
	}

	Ok(())
}

fn update_latest(path: &Path) -> anyhow::Result<()> {
	let dir_filter = |f: Result<fs::DirEntry, _>| f.ok().filter(|f| f.file_type().ok().is_some_and(|f| f.is_dir()));
	let max_name_fold = |acc: u32, e: fs::DirEntry| {
		e.file_name().into_string().map(|s| s.parse::<u32>().unwrap_or(0)).unwrap_or(0).max(acc)
	};

	let paths = fs::read_dir(path)?;
	let year = paths.into_iter().filter_map(dir_filter).fold(0, max_name_fold);
	let year_path = path.join(year.to_string());

	let paths = fs::read_dir(&year_path)?;
	let month = paths.into_iter().filter_map(dir_filter).fold(0, max_name_fold);
	let month_path = year_path.join(format!("{:0>2}", &month));

	let paths = fs::read_dir(&month_path)?;
	let day = paths.into_iter().filter_map(dir_filter).fold(0, max_name_fold);
	let day_path = month_path.join(format!("{:0>2}", &day));

	let latest = path.join(LATEST);
	if latest.exists() {
		if !latest.is_symlink() {
			eprintln!("{} is not a symlink", latest.display());
			return Ok(()); // Do not touch
		}

		fs::remove_file(&latest)?;
	}

	if !day_path.exists() {
		eprintln!("Path found \"{}\" for {}-{}-{} does not exist", day_path.display(), year, month, day);
	} else {
		println!("Symlink: \"{}\" -> \"{}\"", day_path.display(), latest.display());
		std::os::unix::fs::symlink(day_path, latest)?;
	}

	Ok(())
}

fn clean_directory(path: &Path) -> anyhow::Result<()> {
	println!("Started cleaning \"{}\"", path.display());

	// Move all screenshots
	let paths = fs::read_dir(path)?;
	paths
		.into_iter()
		.filter_map(|f| -> Option<(PathBuf, anyhow::Error)> {
			let file = match f {
				Ok(f) => f,
				Err(e) => {
					eprintln!("Error while iterating files: {e}");
					return None;
				}
			};

			if let Err(e) = update_file(path, &file.path()) {
				Some((file.path(), e))
			} else {
				None
			}
		})
		.for_each(|e| eprintln!("Error while processing \"{}\": {}", e.0.display(), e.1));

	// Update latest directory
	update_latest(path)?;

	println!("Cleaning done");

	Ok(())
}

fn move_files(dir: &Path, file: &Path, to: &Path) -> anyhow::Result<()> {
	let from = dir.join(file);
	let to = dir.join(to);
	let end_file = to.join(file);

	println!("Move \"{}\" -> \"{}\"", from.display(), end_file.display());

	if !to.exists() {
		println!("Create \"{}\"", to.display());
		fs::create_dir_all(&to)?;
	}

	fs::rename(&from, &end_file)?;

	Ok(())
}

fn main() {
	// Parse arguments
	let args = Args::parse();

	let screenshot_dir = PathBuf::from(&args.screenshot_dir).canonicalize();

	if let Err(e) = screenshot_dir {
		eprintln!("Could not canonicalize \"{}\": {}", args.screenshot_dir, e);
		std::process::exit(1);
	}

	let screenshot_dir = screenshot_dir.unwrap();

	if !check_exists(&screenshot_dir) {
		eprintln!("Directory \"{}\" does not exist", screenshot_dir.display());
		std::process::exit(1);
	}

	if !screenshot_dir.is_dir() {
		eprintln!("\"{}\" is not a directory", screenshot_dir.display());
		std::process::exit(1);
	}

	// First run cleaning

	if let Err(e) = clean_directory(&screenshot_dir) {
		eprintln!("Error while cleaning directory \"{}\": {e}", screenshot_dir.display());
		std::process::exit(1);
	}

	// Setup watcher

	let signals = Signals::new([SIGINT, SIGTERM]);

	if let Err(e) = signals {
		eprintln!("Error while creating signal handler: {e}");
		std::process::exit(1);
	}

	let mut signals = signals.unwrap();

	let (tx, rx) = std::sync::mpsc::channel();
	let t = tx.clone();

	let running = Arc::new(AtomicBool::new(true));
	let r = running.clone();

	let watcher = notify::RecommendedWatcher::new(tx.clone(), notify::Config::default());

	thread::spawn(move || {
		for sig in signals.forever() {
			match sig {
				SIGINT => {
					println!("CTRL-C received, terminating...");
					r.store(false, Ordering::SeqCst);
					_ = t.send(Err(notify::Error::generic("SIGINT")));
					break;
				}
				SIGTERM => {
					println!("Terminate received, finishing...");
					r.store(false, Ordering::SeqCst);
					_ = t.send(Err(notify::Error::generic("SIGTERM")));
					break;
				}
				_ => (),
			}
		}
	});

	if let Err(e) = watcher {
		eprintln!("Error creating watcher for \"{}\":{e}", screenshot_dir.display());
		std::process::exit(1);
	}

	println!("Watcher starting for \"{}\"", screenshot_dir.display());
	let mut watcher = watcher.unwrap();
	let res = watcher.watch(&screenshot_dir, notify::RecursiveMode::NonRecursive);

	if let Err(e) = res {
		eprintln!("Error watching \"{}\": {e}", screenshot_dir.display());
		std::process::exit(1);
	}

	loop {
		let res = rx.recv();

		if let Err(e) = res {
			eprintln!("Error receiving MPSC message: {e}");
			std::process::exit(1);
		}

		let res = res.unwrap();

		if let Err(e) = res {
			if !running.load(Ordering::SeqCst) {
				// Graceful shutdown
				std::process::exit(0);
			}

			eprintln!("Error with watcher event: {e}");
			std::process::exit(1);
		}

		let event = res.unwrap();

		use notify::event::AccessMode;
		use notify::EventKind;

		if let EventKind::Access(AccessKind::Close(AccessMode::Write)) = event.kind {
			let mut work_done = false;

			for path in event.paths {
				if path.is_file() {
					if let Err(e) = update_file(&screenshot_dir, path.as_path()) {
						eprintln!("Error while handling \"{}\": {e}", path.display());
					}
					work_done = true;
				}
			}

			if work_done {
				if let Err(e) = update_latest(&screenshot_dir) {
					eprintln!("Error while updating \"latest\" link: {e}");
				}
			}
		}
	}
}
