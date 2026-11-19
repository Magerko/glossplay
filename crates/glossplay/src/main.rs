#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod jobs;
mod paths;
mod player;
mod selftest;
mod session;
mod shell;
mod theme;
mod ui;
mod video;
mod ytdlp;

use eframe::egui;
use std::sync::Mutex;
use std::sync::atomic::Ordering;

const HELP: &str = "\
Glossplay — a video player for language learners

Usage: glossplay [FILE or URL] [options]

  --translate-file FILE --to LANG   translate a subtitle file twice and print how many
                                    requests each pass made (the second should make none)
  --selftest                        play FILE without interaction, print a report, exit
      --seconds N                   how long to play (default 6)
      --at SECONDS                  start position
      --subs A,B                    languages of the primary and secondary track;
                                    B = mt shows the machine translation instead
      --autopause                   check auto-pause and repeat on the next line
      --asr                         run speech recognition
      --anki                        save the opened word and send it to Anki
      --strict                      also fail on software decoding or many dropped frames
      --export-srt FILE.srt         write the primary subtitle track to a file, and the
                                    translation shown under it to FILE.tr.srt
      --screenshot FILE.png         save the window as an image at the end
      --controls --sidebar --word N --paused   what to show in the screenshot
  --shell-register                  add Glossplay to Explorer's \"Open with\" and context menu
                                    (current user only; it does not become the default program)
  --shell-unregister                remove those entries
  --update-ytdlp                 download or update yt-dlp and print its version
  --size WxH                        window size
  --version, --help";

fn main() {
    let all: Vec<String> = std::env::args().collect();
    if all.get(1).map(String::as_str) == Some("--watchdog") {
        let arg = |i: usize| all.get(i).map_or("", String::as_str);
        std::process::exit(gp_media::tools::watchdog(arg(2), arg(3)));
    }
    if let Ok(exe) = std::env::current_exe() {
        gp_media::tools::enable_watchdog(exe);
    }
    let mut args = std::env::args().skip(1);
    let mut media = None;
    let mut plan = selftest::Plan {
        seconds: 6.0,
        ..Default::default()
    };
    let mut is_selftest = false;
    let mut size = [1280.0, 720.0];
    let (mut translate_file, mut to_lang) = (None, None);
    while let Some(arg) = args.next() {
        let mut value = || args.next().unwrap_or_default();
        match arg.as_str() {
            "--help" | "-h" => return println!("{HELP}"),
            "--version" | "-V" => return println!("glossplay {}", env!("CARGO_PKG_VERSION")),
            "--selftest" => is_selftest = true,
            "--seconds" => plan.seconds = value().parse().unwrap_or(6.0),
            "--at" => plan.at = value().parse().ok(),
            "--subs" => plan.subs = value().split(',').map(str::to_owned).collect(),
            "--screenshot" => plan.screenshot = Some(value().into()),
            "--controls" => plan.show_controls = true,
            "--sidebar" => plan.sidebar = true,
            "--paused" => plan.paused = true,
            "--word" => plan.word = value().parse().ok(),
            "--autopause" => plan.autopause = true,
            "--asr" => plan.asr = true,
            "--anki" => plan.anki = true,
            "--strict" => plan.strict = true,
            "--export-srt" => plan.export_srt = Some(value().into()),
            "--shell-register" => std::process::exit(shell::cli(true)),
            "--shell-unregister" => std::process::exit(shell::cli(false)),
            "--update-ytdlp" => {
                return match ytdlp::ensure(false) {
                    Ok(outcome) => println!("{outcome}\nversion={}", outcome.version()),
                    Err(e) => {
                        eprintln!("{e}");
                        std::process::exit(1);
                    }
                };
            }
            "--translate-file" => translate_file = Some(value()),
            "--to" => to_lang = Some(value()),
            "--size" => {
                let v = value();
                if let Some((w, h)) = v.split_once('x') {
                    size = [w.parse().unwrap_or(1280.0), h.parse().unwrap_or(720.0)];
                }
            }
            _ => media = Some(arg),
        }
    }
    if let Some(file) = translate_file {
        std::process::exit(translate_file_twice(&file, to_lang.as_deref()));
    }

    let launch = app::Launch {
        media,
        selftest: is_selftest.then_some(plan),
    };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Glossplay")
            .with_inner_size(size)
            .with_min_inner_size([640.0, 360.0])
            .with_drag_and_drop(true),
        renderer: eframe::Renderer::Glow,
        ..Default::default()
    };
    let is_selftest = launch.selftest.is_some();
    let launch = Mutex::new(Some(launch));
    let result = eframe::run_native(
        "Glossplay",
        options,
        Box::new(move |cc| {
            let launch = launch
                .lock()
                .unwrap()
                .take()
                .expect("the app is created once");
            match app::App::new(cc, launch) {
                Ok(app) => Ok(Box::new(app) as Box<dyn eframe::App>),
                Err(message) => {
                    eprintln!("{message}");
                    if is_selftest {
                        // Nobody is there to read a window; report on stderr and stop.
                        std::process::exit(2);
                    }
                    selftest::EXIT_CODE.store(2, Ordering::SeqCst);
                    Ok(Box::new(StartupError { message }))
                }
            }
        }),
    );
    // `on_exit` has stopped the server already when the window closed normally; this also
    // covers leaving any other way, since `process::exit` runs no destructors.
    gp_media::tools::stop_all();
    if let Err(e) = result {
        eprintln!("{e}");
        std::process::exit(2);
    }
    std::process::exit(selftest::EXIT_CODE.load(Ordering::SeqCst));
}

/// Shown instead of the player when it cannot start, most often because libmpv is missing.
struct StartupError {
    message: String,
}

impl eframe::App for StartupError {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ui, |ui| {
            ui.add_space(40.0);
            ui.vertical_centered(|ui| {
                ui.heading("Glossplay could not start");
                ui.add_space(12.0);
                ui.label(&self.message);
                ui.add_space(12.0);
                ui.label("Put the mpv library (libmpv) next to the program, or set player.mpv_path in config.toml:");
                ui.monospace(paths::config_path().to_string_lossy());
            });
        });
    }
}

fn translate_file_twice(file: &str, to: Option<&str>) -> i32 {
    let (cfg, config_error) = paths::load_config();
    if let Some(e) = config_error {
        eprintln!("{e}; using the default settings");
    }
    let run = || -> Result<(), String> {
        let bytes = std::fs::read(file).map_err(|e| e.to_string())?;
        let hint = file
            .rsplit('.')
            .next()
            .and_then(gp_core::SubFormat::from_extension);
        let cues = gp_core::parse_subtitles(&bytes, hint).map_err(|e| e.to_string())?;
        let lines: Vec<String> = cues.iter().map(|c| c.text.clone()).collect();
        let translator =
            gp_ai::translate::build_translator(&cfg.translation).map_err(|e| e.to_string())?;
        let store =
            Mutex::new(gp_store::Store::open(&paths::db_path()).map_err(|e| e.to_string())?);
        let dst = to.unwrap_or(&cfg.general.native_lang);
        println!("provider={}", translator.id());
        println!("lines={}", lines.len());
        for pass in 1..=2 {
            let out = jobs::translate_cached(&store, translator.as_ref(), None, dst, &lines, &[])
                .map_err(|e| e.to_string())?;
            let filled = out.results.iter().filter(|r| !r.trim().is_empty()).count();
            println!("pass{pass}_requests={}", out.requests);
            println!("pass{pass}_translated={filled}");
            if pass == 1 {
                for (src, dst) in lines.iter().zip(&out.results).take(3) {
                    println!("sample={} => {}", src.replace('\n', " "), dst);
                }
            }
        }
        Ok(())
    };
    match run() {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("{e}");
            1
        }
    }
}
