//! The apps: `erisg1 v0 app NAME [ARGS]` runs one on the glasses, `erisg1 v0 preview NAME` on two simulated arms,
//! recording what each eye would show to NAME.png (an animated PNG). `erisg1 v0 apps` lists them.
//!
//! Each app is a module with an `APP`: its name, what it does, the kit options it needs, the function that runs it
//! (against `kit::Glasses`, real or simulated alike), and the script its preview follows.

pub(crate) mod kit;
pub(crate) mod sim;
pub(crate) mod vmkit;

mod breathe;
mod bricks;
mod cards;
mod dodge;
mod fireworks;
mod fun;
mod metronome;
mod note;
mod notify;
mod nowplaying;
mod pomodoro;
mod posture;
mod power;
mod prompter;
mod run;
mod sketch;
mod steps;
mod stopwatch;
pub(crate) mod sysmon;
mod warp;
pub(crate) mod weather;

use std::future::Future;
use std::path::Path;
use std::pin::Pin;

use kit::{Glasses, Options, Target};
use sim::Script;

use crate::link::Error;
use crate::proto::Side;

pub(crate) type Running<'a> = Pin<Box<dyn Future<Output = Result<(), Error>> + 'a>>;

pub(crate) struct App {
    pub name: &'static str,
    /// One line on what it does and how it's used.
    pub about: &'static str,
    /// One of the "super fun" ones rather than the useful ones.
    pub fun: bool,
    pub options: Options,
    /// Runs it until it's done (or for a program that runs on the glasses, until it's started), with the
    /// arguments after its name.
    pub run: for<'a> fn(&'a mut Glasses, &'a [String]) -> Running<'a>,
    /// What happens in its preview, and the arguments it's given there.
    pub demo: fn() -> Script,
    pub demo_args: &'static [&'static str],
}

pub(crate) fn all() -> Vec<App> {
    vec![
        notify::APP,
        run::APP,
        sysmon::APP,
        nowplaying::APP,
        weather::APP,
        power::APP,
        stopwatch::APP,
        pomodoro::APP,
        posture::APP,
        prompter::APP,
        cards::APP,
        note::APP,
        steps::APP,
        metronome::APP,
        breathe::APP,
        warp::APP,
        dodge::APP,
        bricks::APP,
        sketch::APP,
        fireworks::APP,
    ]
}

fn find(name: &str) -> Result<App, Error> {
    all()
        .into_iter()
        .find(|a| a.name == name)
        .ok_or_else(|| format!("no app {name}; `erisg1 v0 apps` lists them").into())
}

pub async fn say(text: &str) -> Result<(), Error> {
    notify::say(text).await
}

pub async fn list() -> Result<(), Error> {
    for (title, fun) in [("useful", false), ("super fun", true)] {
        println!("{title}:");
        for app in all().into_iter().filter(|a| a.fun == fun) {
            println!("  {:<12} {}", app.name, app.about);
        }
    }
    Ok(())
}

pub async fn run(name: &str, side: Option<Side>, args: &[String]) -> Result<(), Error> {
    let app = find(name)?;
    let mut glasses = Glasses::open(Target::Glasses(side), app.options).await?;
    let result = (app.run)(&mut glasses, args).await;
    let closed = glasses.close().await;
    result.and(closed)
}

/// Runs `name` (or every app, for "all") on the simulated arms and writes `NAME.png` into `out`.
pub async fn preview(name: &str, out: &Path) -> Result<(), Error> {
    std::fs::create_dir_all(out).map_err(|e| format!("{}: {e}", out.display()))?;
    let apps: Vec<App> = if name == "all" { all() } else { vec![find(name)?] };
    let mut failed = Vec::new();
    for app in apps {
        let path = out.join(format!("{}.png", app.name));
        let args: Vec<String> = app.demo_args.iter().map(|s| s.to_string()).collect();
        let result = async {
            let mut glasses = Glasses::open(Target::Sim((app.demo)(), Some(path.clone())), app.options).await?;
            (app.run)(&mut glasses, &args).await?;
            glasses.close().await
        }
        .await;
        match result {
            Ok(()) => println!("{:<12} {}", app.name, path.display()),
            Err(e) => {
                println!("{:<12} failed: {e}", app.name);
                failed.push(app.name);
            }
        }
    }
    if failed.is_empty() { Ok(()) } else { Err(format!("failed: {}", failed.join(", ")).into()) }
}
